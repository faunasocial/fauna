using System.Linq;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using uniffi.fauna_core;
using uniffi.fauna_feed;
using uniffi.fauna_ffi;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Windows consumes the shared <c>fauna-feed</c> post-source classifier through
/// the <c>fauna-ffi</c> <c>classify_sources</c> façade (native <c>fauna_ffi</c> dll
/// loads in the test host — memory <c>reference_windows_dotnet_test_loads_native_ffi</c>)
/// instead of a per-app <c>source -&gt; badge</c> switch
/// (<c>docs/goal/ui/feed.md</c> § Where logic lives; priority #1/#2/#4). These lock
/// windows to the canonical ids + labels and the multi-source-per-origin behavior.
/// The wrapper is now built from the shared <c>PostSummary</c> snapshot record
/// (the lift onto <c>FfiFeedManager</c>); <c>ForSourceTest</c> mints one from just
/// the source string.
/// </summary>
[Collection("StringsGlobal")]
public class FeedPostItemTests
{
    [Fact]
    public void MultiSource_RendersOneBadgePerOriginInOrder()
    {
        var item = FeedPostItem.ForSourceTest("fauna, bluesky");

        Assert.Equal(2, item.SourceBadges.Count);
        Assert.Equal("fauna", item.SourceBadges[0].Id);
        Assert.Equal("Fauna", item.SourceBadges[0].Label);
        Assert.Equal("bluesky", item.SourceBadges[1].Id);
        Assert.Equal("Bluesky", item.SourceBadges[1].Label);
        // The badge carries the shared SourceGlyph emoji (FfiSourceBadge.glyph →
        // SourceGlyphAsset.Emoji — render-model.md § D5; the SAME map the rail uses,
        // so badge and rail can't drift). Locks the end-to-end native classify chain.
        Assert.Equal("\U0001F98A", item.SourceBadges[0].Glyph); // fauna → fox 🦊
        Assert.Equal("\U0001F98B", item.SourceBadges[1].Glyph); // bluesky → butterfly 🦋
    }

    [Fact]
    public void ActivityPub_UsesCanonicalFediverseLabel()
    {
        var item = FeedPostItem.ForSourceTest("activitypub");

        var badge = Assert.Single(item.SourceBadges);
        Assert.Equal("activitypub", badge.Id);
        // The canonical shared label — windows must not re-hardcode "ActivityPub"/"ap".
        Assert.Equal("Fediverse", badge.Label);
        // Fediverse renders the generic globe (not the Mastodon elephant) — render-model.md § D5.
        Assert.Equal("\U0001F310", badge.Glyph); // activitypub → globe 🌐
    }

    [Fact]
    public void EmptySource_RendersNoBadge()
    {
        var item = FeedPostItem.ForSourceTest("");

        Assert.Empty(item.SourceBadges);
    }

    // ── Feed body adopts the shared RenderDocument (P3/D6, render-model.md § D6) ──

    /// <summary>A plain-text body document carries no blocked remote image, so the
    /// per-card load-remote-content-button stays hidden (the common case — most posts
    /// have no remote `![]()` in their markdown).</summary>
    [Fact]
    public void PlainBody_HasNoBlockedRemoteImage()
    {
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("hello world") }),
        })));

        Assert.False(item.HasBlockedRemoteImage);
    }

    /// <summary>A body with a <c>RemoteImage(revealed: false)</c> block (the manager
    /// default — not yet revealed) gates the load-remote-content-button on. A post where
    /// the manager has already projected <c>revealed: true</c> onto the block hides the
    /// button — D3: the manager is the authority for reveal state (render-model.md § D3;
    /// html-mail.md § Rendering + § Security &amp; privacy).</summary>
    [Fact]
    public void RemoteImageBody_GatesRevealButtonUntilManagerReveals()
    {
        // Pre-reveal: manager projects revealed=false (the default blocked posture).
        var blocked = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.RemoteImage("https://img.test/c.png", "a grey cat", false),
        })));
        Assert.True(blocked.HasBlockedRemoteImage);

        // Post-reveal: the manager flips the set + re-emits; Refresh rebuilds the item
        // from the new snapshot where revealed=true — the button gate must be false.
        var revealed = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.RemoteImage("https://img.test/c.png", "a grey cat", true),
        })));
        Assert.False(revealed.HasBlockedRemoteImage);
    }

    // ── D6 embed-fold: quote + media derived from the folded document (render-model.md § D6) ──
    // The feed manager folds a QuotedPost block (resolve_quoted_post) and an Image block
    // (resolve_media) into PostSummary.document; the wrapper derives the quoted-post card +
    // the media hash from those blocks (no sibling quoted_post_id / media_hash render),
    // matching linux, web, and android. The quoted author is the shared ShortId form, like the
    // pre-fold ResolveQuotedAsync did.

    [Fact]
    public void QuotedPostBlock_DerivesQuoteCardFromDocument()
    {
        var author = new string('a', 64);
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("look at this") }),
            // Slice 2b added `verification` as the 4th field; Unchecked keeps this
            // existing assertion unchanged. The windows badge render leg
            // adds a Failed-case test for the quoted-post `unverified-source-badge`.
            RenderBlockFixture.QuotedPost("quoted-id", author, "the quoted body"),
        })));

        Assert.True(item.HasQuotedPost);
        // Windows applies the shared short-id form to the quoted author, not the raw hex.
        Assert.Equal(FaunaFfiMethods.ShortId(author), item.QuotedPostAuthor);
        Assert.Equal("the quoted body", item.QuotedPostBody);
    }

    [Fact]
    public void NoQuotedPostBlock_HasNoQuoteCard()
    {
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("plain") }),
        })));

        Assert.False(item.HasQuotedPost);
        Assert.Equal("", item.QuotedPostAuthor);
        Assert.Equal("", item.QuotedPostBody);
    }

    // ── Legal-takedown quoted post (moderation.md § Categories & enforcement item 1) ──
    // When the quoted post has been taken down under a legal obligation, the shared
    // FeedManager::resolve_quoted_post projects a tombstone QuotedPostView (empty
    // body/author) carrying legal_takedown_ref. Windows paints the shared localized
    // tombstone (FaunaFfiMethods.LegalTakedownTombstone via Strings.Resolve — NO
    // hand-rolled string) in place of the withheld body + omits the author/unverified
    // rows, mirroring web QuotedPost.svelte's `legal_takedown_ref` arm + linux
    // document.rs's build_quoted_post_card branch (priority #1/#2).

    [Fact]
    public void QuotedPostLegalTakedown_PaintsSharedTombstoneNotBody()
    {
        const string reference = "EU-DSA-2024/12345";
        // The tombstone projection withholds body+author (empty), but the takedown
        // branch must win over any body: pass a sentinel body and assert it's replaced.
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("look at this") }),
            RenderBlockFixture.QuotedPost("quoted-id", "", "should-not-render",
                legalTakedownRef: reference),
        })));

        Assert.True(item.HasQuotedPost);
        Assert.True(item.QuotedPostIsLegalTakedown);
        // Conformance: the painted body is EXACTLY the shared FFI tombstone face's
        // resolved text — a hand-rolled string would diverge from this.
        Assert.Equal(
            Strings.Resolve(FaunaFfiMethods.LegalTakedownTombstone(reference)),
            item.QuotedPostDisplayBody);
        // The withheld body is never shown, and the author/unverified header is omitted.
        Assert.NotEqual("should-not-render", item.QuotedPostDisplayBody);
        Assert.False(item.QuotedPostShowHeaderRow);
    }

    // A quote whose target its author deleted (ui/feed.md § Post deletion: references
    // dangle by design) — the shared resolve folds `not_found`, and the card paints
    // `feed.post.post_not_found` where it paints the takedown tombstone, never a blank
    // embed (web QuotedPost.svelte's not_found arm, linux document.rs's placeholder).
    [Fact]
    public void QuotedPostNotFound_PaintsPostNotFoundWithoutHeader()
    {
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            RenderBlockFixture.QuotedPost("deleted-id", "", "", notFound: true),
        })));

        Assert.True(item.HasQuotedPost);
        Assert.True(item.QuotedPostIsNotFound);
        Assert.Equal(Strings.Get("feed/post/post_not_found"), item.QuotedPostDisplayBody);
        Assert.False(item.QuotedPostShowHeaderRow);
        // The empty author would prune the Border's UIA peer; the placeholder names it.
        Assert.Equal(item.QuotedPostDisplayBody, item.QuotedPostAccessibleName);
    }

    // feed.md § Interaction bar: "count is hidden when 0". The button's Name is what the
    // automation read (and a screen reader) gets for an icon + count button, so it names
    // the verb and carries a number only while the count is shown.
    [Fact]
    public void InteractionButtonName_CarriesTheCountOnlyAboveZero()
    {
        var fresh = new FeedPostItem(PostSummaryFixture.Make(postId: "p", author: "a"));
        Assert.Equal(Strings.Get("feed/like_tooltip"), fresh.LikeButtonName);
        Assert.DoesNotContain(fresh.ReplyButtonName, char.IsDigit);
        Assert.DoesNotContain(fresh.RepostButtonName, char.IsDigit);
        Assert.DoesNotContain(fresh.QuoteButtonName, char.IsDigit);

        var liked = new FeedPostItem(PostSummaryFixture.Make(postId: "p", author: "a", likeCount: 3));
        Assert.Equal($"{Strings.Get("feed/like_tooltip")} 3", liked.LikeButtonName);
        Assert.DoesNotContain(liked.ReplyButtonName, char.IsDigit);
    }

    [Fact]
    public void QuotedPostNormal_PaintsBodyWithHeader()
    {
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            RenderBlockFixture.QuotedPost("quoted-id", new string('a', 64), "the quoted body"),
        })));

        Assert.False(item.QuotedPostIsLegalTakedown);
        Assert.False(item.QuotedPostIsNotFound);
        Assert.Equal("the quoted body", item.QuotedPostDisplayBody);
        Assert.True(item.QuotedPostShowHeaderRow);
    }

    [Fact]
    public void ImageBlock_DerivesMediaHashFromDocument()
    {
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("photo") }),
            new RenderBlock.Image("deadbeef", ""),
        })));

        Assert.Equal("deadbeef", item.MediaHashHex);
    }

    [Fact]
    public void NoImageBlock_HasEmptyMediaHash()
    {
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("no media") }),
        })));

        Assert.Equal("", item.MediaHashHex);
    }

    // ── Video block folded into the document (render-model.md § Implementation status
    // today — the D6b typed sibling of Image), the exact twin of the ImageBlock pair above.

    [Fact]
    public void VideoBlock_DerivesVideoHashFromDocument()
    {
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("clip") }),
            new RenderBlock.Video("feedbeef", ""),
        })));

        Assert.Equal("feedbeef", item.VideoHashHex);
        Assert.True(item.HasVideo);
    }

    [Fact]
    public void NoVideoBlock_HasEmptyVideoHashAndNotHasVideo()
    {
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("no video") }),
        })));

        Assert.Equal("", item.VideoHashHex);
        Assert.False(item.HasVideo);
    }

    [Fact]
    public void ContentEquals_VideoFold_False()
    {
        // The video twin of ContentEquals_MediaFold_False below: a Video block folding in
        // must repaint the row so video-thumbnail appears.
        var unresolved = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("clip") }),
        })));
        var resolved = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("clip") }),
            new RenderBlock.Video("feedbeef", ""),
        })));
        Assert.False(unresolved.ContentEquals(resolved));
    }

    // ── Content-label badge (moderation.md § Per-row badge data path) ──

    [Fact]
    public void NoLabels_NoBadge()
    {
        var item = new FeedPostItem(MakeBodyPost(VerificationStatus.Unchecked));
        Assert.False(item.HasContentLabel);
        Assert.Equal("", item.ContentLabelText);
    }

    [Fact]
    public void OneLabel_RendersBadgeFromSharedStyle()
    {
        var item = new FeedPostItem(MakeLabeledPost(
            new ContentLabelEntry(@category: "spam", @confidencePerMille: 900)));

        Assert.True(item.HasContentLabel);
        // Shared fauna_core::content_category::content_label_style map — locks windows
        // to the canonical presentation, not a per-app string/colour.
        Assert.Equal("#EF4444", item.ContentLabelTint);
        Assert.Equal("#DC2626", item.ContentLabelAccent);
        Assert.NotEmpty(item.ContentLabelIcon);
        Assert.NotEmpty(item.ContentLabelText);
    }

    [Fact]
    public void MultipleLabels_PicksHighestConfidenceEntry()
    {
        // The shared primary_content_label reduce — nsfw wins on confidence even though
        // spam is listed first (no per-app "first wins" re-derivation).
        var item = new FeedPostItem(MakeLabeledPost(
            new ContentLabelEntry(@category: "spam", @confidencePerMille: 400),
            new ContentLabelEntry(@category: "nsfw", @confidencePerMille: 900)));

        Assert.True(item.HasContentLabel);
        Assert.Equal("#F97316", item.ContentLabelTint); // nsfw's tint, not spam's
    }

    [Fact]
    public void OffListCategory_DegradesToGreyOther()
    {
        // A legal-takedown-style off-list category must never crash or drop the row —
        // it degrades to the neutral grey Other badge (moderation.md § Where logic lives).
        var item = new FeedPostItem(MakeLabeledPost(
            new ContentLabelEntry(@category: "illegal", @confidencePerMille: 1000)));

        Assert.True(item.HasContentLabel);
        Assert.Equal("#6B7280", item.ContentLabelTint);
    }

    private static PostSummary MakeLabeledPost(params ContentLabelEntry[] labels) =>
        PostSummaryFixture.Make(document: EmptyDoc(), labels: labels);

    // ── Unverified-source badge gate (security.md § App display of unverified content) ──
    // IsUnverifiedSource drives the muted unverified-source-badge: true ONLY for
    // VerificationStatus.Failed; Unchecked (the feed-list default, no envelope to verify)
    // and Verified both render no badge.

    // VerificationStatus is UniFFI-internal, so it can't be a public [Theory] param
    // (CS0051) — three [Fact]s constructing the enum internally, matching the
    // param-less style of the other tests in this file.

    [Fact]
    public void IsUnverifiedSource_Failed_ShowsBadge()
    {
        var item = new FeedPostItem(MakeBodyPost(VerificationStatus.Failed));
        Assert.True(item.IsUnverifiedSource);
    }

    [Fact]
    public void IsUnverifiedSource_Unchecked_NoBadge()
    {
        // The feed-list default — no envelope to verify, so never a badge.
        var item = new FeedPostItem(MakeBodyPost(VerificationStatus.Unchecked));
        Assert.False(item.IsUnverifiedSource);
    }

    [Fact]
    public void IsUnverifiedSource_Verified_NoBadge()
    {
        var item = new FeedPostItem(MakeBodyPost(VerificationStatus.Verified));
        Assert.False(item.IsUnverifiedSource);
    }

    // ── Slice 2b: the unverified-source badge on the QUOTED-post embed
    // (security.md § App display of unverified content). QuotedPostIsUnverified
    // is driven by the QUOTED block's own verification (RenderBlock.QuotedPost.verification
    // == Failed), independent of the quoting post's own IsUnverifiedSource. ──

    [Fact]
    public void QuotedPostIsUnverified_FailedQuote_ShowsBadge()
    {
        var item = new FeedPostItem(MakeQuotedPost(VerificationStatus.Failed));
        Assert.True(item.QuotedPostIsUnverified);
        // The quoting post itself is Unchecked — only the embedded quote failed.
        Assert.False(item.IsUnverifiedSource);
    }

    [Fact]
    public void QuotedPostIsUnverified_UncheckedQuote_NoBadge()
    {
        var item = new FeedPostItem(MakeQuotedPost(VerificationStatus.Unchecked));
        Assert.False(item.QuotedPostIsUnverified);
    }

    [Fact]
    public void QuotedPostIsUnverified_VerifiedQuote_NoBadge()
    {
        var item = new FeedPostItem(MakeQuotedPost(VerificationStatus.Verified));
        Assert.False(item.QuotedPostIsUnverified);
    }

    [Fact]
    public void QuotedPostIsUnverified_NoQuote_IsFalse()
    {
        // Even a Failed quoting post has no quoted-embed badge when it quotes nothing.
        var item = new FeedPostItem(MakeBodyPost(VerificationStatus.Failed));
        Assert.False(item.QuotedPostIsUnverified);
    }

    // ── D4 link-preview card text (render-model.md § D4) ─────────────────────
    // The producer's bare-URL rule folds a RenderBlock.LinkPreview block into
    // PostSummary.document; the card's text (title/description/domain) is derived
    // from the Resolved PreviewState. Resolving/Failed paint no card — the kept
    // inline link shows the URL (matches the web reference — no skeleton). The
    // Resolving→Resolved trigger is now wired (FeedPage.SyncPosts → the shared
    // FeedManager::resolve_link_preview), so Resolving is transient.
    // The og:image (`link-preview-image`) is REVEAL-GATED on all 6 (render-model.md
    // § D4, user-ratified 2026-06-27): blocked-by-default like any RemoteImage, it
    // paints only after the post's load-remote-content-button reveals it, and its hash
    // (the fetch target) is withheld until then (the card never fetches before reveal).
    //
    // PreviewState is UniFFI-internal, so the card exposes public primitives, not
    // the state record; these [Fact]s construct it internally (InternalsVisibleTo).

    [Fact]
    public void LinkPreviewResolved_DerivesCardTextFromDocument()
    {
        var item = new FeedPostItem(MakeLinkPreviewPost(
            new PreviewState.Resolved("My Title", "A short description", "imghash", revealed: false)));

        var card = Assert.Single(item.LinkPreviewCards);
        Assert.Equal("https://example.com/article", card.Url);
        Assert.Equal("My Title", card.Title);
        Assert.Equal("A short description", card.Description);
        // Domain via the shared fauna_core::format::url_host (FaunaFfiMethods.UrlHost),
        // not a per-app parse — render-model.md § D4 (priority #1/#2).
        Assert.Equal("example.com", card.Domain);
    }

    [Fact]
    public void LinkPreviewResolvedRevealed_ExposesOgImageHashAndShows()
    {
        // Once the post's remote content is revealed, the og:image paints and its hash
        // (the /api/v1/blob/<hash> fetch target) is exposed for the card's image loader.
        var item = new FeedPostItem(MakeLinkPreviewPost(
            new PreviewState.Resolved("My Title", "A short description", "imghash", revealed: true)));

        var card = Assert.Single(item.LinkPreviewCards);
        Assert.True(card.ImageRevealed);
        Assert.True(card.ShowImage);
        Assert.Equal("imghash", card.ImageHash);
    }

    [Fact]
    public void LinkPreviewResolvedBlocked_HidesOgImageAndWithholdsHash()
    {
        // Blocked-by-default (D4 reveal gate, user-ratified 2026-06-27): the text card paints,
        // but the og:image is hidden and its hash is NOT exposed until the post is revealed —
        // the card must never fetch the blob before reveal (privacy posture, the D3 twin).
        var item = new FeedPostItem(MakeLinkPreviewPost(
            new PreviewState.Resolved("My Title", "A short description", "imghash", revealed: false)));

        var card = Assert.Single(item.LinkPreviewCards);
        Assert.False(card.ImageRevealed);
        Assert.False(card.ShowImage);
        Assert.Equal("", card.ImageHash);
    }

    [Fact]
    public void LinkPreviewResolvedNoImage_NeverShowsOgImage()
    {
        // A Resolved preview with no og:image (image_hash null) → no image even when revealed.
        var item = new FeedPostItem(MakeLinkPreviewPost(
            new PreviewState.Resolved("My Title", "A short description", null, revealed: true)));

        var card = Assert.Single(item.LinkPreviewCards);
        Assert.False(card.ShowImage);
        Assert.Equal("", card.ImageHash);
    }

    [Fact]
    public void LinkPreviewResolving_RendersNoCardYet()
    {
        // A Resolving block paints no card — the kept inline link shows the URL
        // (matches the web reference; no skeleton). The trigger (FeedPage.SyncPosts)
        // flips it to Resolved/Failed, which then paints (or stays the inline link).
        var item = new FeedPostItem(MakeLinkPreviewPost(new PreviewState.Resolving()));

        Assert.Empty(item.LinkPreviewCards);
    }

    [Fact]
    public void LinkPreviewFailed_RendersNoCard()
    {
        // Failed → fall back to the plain inline link, no card (render-model.md § D4).
        var item = new FeedPostItem(MakeLinkPreviewPost(new PreviewState.Failed()));

        Assert.Empty(item.LinkPreviewCards);
    }

    [Fact]
    public void NoLinkPreviewBlock_HasNoCard()
    {
        var item = new FeedPostItem(MakeBodyPost(VerificationStatus.Unchecked));

        Assert.Empty(item.LinkPreviewCards);
    }

    [Fact]
    public void TwoBareUrls_ProjectACardEach_AndContentEqualsSeesTheSecond()
    {
        // The producer emits a preview per standalone bare-URL paragraph, so the item carries
        // BOTH — windows painted only the first until 2026-08-02 (render-model.md
        // § Implementation status). ContentEquals must also notice a change confined to the
        // SECOND card, which the eight flat per-post comparisons it replaced could not see:
        // that is what makes the observer tick actually repaint a late-resolving second card.
        var twoCards = MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("two links follow") }),
            new RenderBlock.LinkPreview("https://first.example/a",
                new PreviewState.Resolved("First", "desc one", null, revealed: false)),
            new RenderBlock.LinkPreview("https://second.example/b",
                new PreviewState.Resolved("Second", "desc two", null, revealed: false)),
        }));
        var item = new FeedPostItem(twoCards);

        Assert.Equal(2, item.LinkPreviewCards.Count);
        Assert.Equal(new[] { "First", "Second" }, item.LinkPreviewCards.Select(c => c.Title));

        var secondResolvedLater = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("two links follow") }),
            new RenderBlock.LinkPreview("https://first.example/a",
                new PreviewState.Resolved("First", "desc one", null, revealed: false)),
            new RenderBlock.LinkPreview("https://second.example/b",
                new PreviewState.Resolved("Second EDITED", "desc two", null, revealed: false)),
        })));

        Assert.False(item.ContentEquals(secondResolvedLater));
    }

    [Fact]
    public void APreviewThatFails_IsAContentChange_ThoughNoCardPaintsEitherWay()
    {
        // `Resolving` and `Failed` paint the same (the inline link, no card), so a row kept
        // on the cards alone would keep saying `resolving` to the e2e dump after the nest
        // refused the preview (render-model.md § D4; test_feed_link_preview_failed.py).
        RenderDocument Doc(PreviewState state) => new(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("marker") }),
            new RenderBlock.LinkPreview("http://10.0.0.5/", state),
        });
        var resolving = new FeedPostItem(MakePost(Doc(new PreviewState.Resolving())));
        var failed = new FeedPostItem(MakePost(Doc(new PreviewState.Failed())));

        Assert.Empty(resolving.LinkPreviewCards);
        Assert.Empty(failed.LinkPreviewCards);
        Assert.Equal(new[] { new LinkPreviewStateRow("http://10.0.0.5/", "failed") },
            failed.LinkPreviewStates);
        Assert.False(resolving.ContentEquals(failed));
    }

    // ── D3 body remote images (render-model.md § D3 + § Implementation status;
    // apps/tui.md § Rendering) — the fifth lifted embed-projection, mirroring the
    // LinkPreviewCards group above: FeedPostItem.RemoteImages is derived through the
    // shared RemoteImageCardModel (RenderDocument::remote_images), not a local walk.

    [Fact]
    public void RemoteImage_DerivesCardFromDocument()
    {
        var item = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("see") }),
            new RenderBlock.RemoteImage("https://img.test/c.png", "a grey cat", revealed: false),
        })));

        var card = Assert.Single(item.RemoteImages);
        Assert.Equal("https://img.test/c.png", card.Url);
        Assert.Equal("a grey cat", card.Alt);
        Assert.False(card.Revealed);
    }

    [Fact]
    public void NoRemoteImageBlock_HasNoCard()
    {
        var item = new FeedPostItem(MakeBodyPost(VerificationStatus.Unchecked));

        Assert.Empty(item.RemoteImages);
    }

    [Fact]
    public void TwoRemoteImages_ProjectACardEach_AndContentEqualsSeesTheSecond()
    {
        // A reveal flips ALL blocked images in the body at once (the one per-post reveal
        // set), so a two-image post must carry both cards and ContentEquals must notice a
        // change confined to the SECOND — the same completeness bar TwoBareUrls proves above.
        var twoImages = MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("two images follow") }),
            new RenderBlock.RemoteImage("https://first.example/a.png", "first", revealed: false),
            new RenderBlock.RemoteImage("https://second.example/b.png", "second", revealed: false),
        }));
        var item = new FeedPostItem(twoImages);

        Assert.Equal(2, item.RemoteImages.Count);
        Assert.Equal(new[] { "first", "second" }, item.RemoteImages.Select(c => c.Alt));

        var secondRevealed = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("two images follow") }),
            new RenderBlock.RemoteImage("https://first.example/a.png", "first", revealed: false),
            new RenderBlock.RemoteImage("https://second.example/b.png", "second", revealed: true),
        })));

        Assert.False(item.ContentEquals(secondRevealed));
    }

    private static PostSummary MakeLinkPreviewPost(PreviewState state) =>
        MakePost(new RenderDocument(new RenderBlock[]
        {
            // The producer keeps the inline link in the paragraph and adds the
            // LinkPreview as an additional top-level block (render.rs bare-URL rule);
            // the card derivation reads the block, so a plain paragraph suffices here.
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("check this out") }),
            new RenderBlock.LinkPreview("https://example.com/article", state),
        }));

    private static PostSummary MakeQuotedPost(VerificationStatus quotedVerification) =>
        MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("look at this") }),
            RenderBlockFixture.QuotedPost("quoted-id", new string('a', 64), "the quoted body",
                quotedVerification),
        }));

    private static PostSummary MakeBodyPost(VerificationStatus verification) =>
        MakePost(
            new RenderDocument(new RenderBlock[]
            {
                new RenderBlock.Paragraph(new Inline[] { new Inline.Text("body") }),
            }),
            verification);

    /// <summary>Mint a <see cref="PostSummary"/> carrying <paramref name="document"/> as
    /// its body render projection; the other fields are inert defaults (source "fauna" so
    /// the shared classifier returns one badge — the FeedPostItem ctor runs it).</summary>
    private static PostSummary MakePost(
        RenderDocument document,
        VerificationStatus verification = VerificationStatus.Unchecked) =>
        PostSummaryFixture.Make(document: document, verification: verification);

    // ── Interaction-bar counts (feed.md § Interaction bar, ratified 2026-06-27) ──

    /// <summary>The icon+count interaction bar binds the REAL snapshot counts
    /// (PostSummary.{like,reply,repost,quote}_count) — not the pre-lift hardcoded 0 —
    /// so the like/reply/repost/quote buttons show the post's real activity. The count
    /// is hidden at 0 in XAML (FeedPage.CountToVisibility); this locks the value flow.</summary>
    [Fact]
    public void InteractionCounts_BindRealSnapshotValues()
    {
        var p = PostSummaryFixture.Make(
            document: EmptyDoc(),
            likeCount: 5, replyCount: 4, repostCount: 3, quoteCount: 2);

        var item = new FeedPostItem(p);

        Assert.Equal(5, item.LikeCount);
        Assert.Equal(4, item.ReplyCount);
        Assert.Equal(3, item.RepostCount);
        Assert.Equal(2, item.QuoteCount);
    }

    /// <summary>The like button's lit state binds PostSummary.viewer_liked — what
    /// FeedPage.LikedToButtonStyle paints and what routes the next tap to like vs.
    /// unlike (feed.md § Interaction bar → Repost, ratified 2026-08-10).</summary>
    [Fact]
    public void ViewerLiked_BindsRealSnapshotValue()
    {
        var liked = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc(), viewerLiked: true));
        var unliked = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc(), viewerLiked: false));

        Assert.True(liked.ViewerLiked);
        Assert.False(unliked.ViewerLiked);
    }

    // ── Repost render + toggle (feed.md § Interaction bar → Repost, ratified
    // 2026-08-10) — the twin of the like-toggle pair above.
    // IsRepostRow (RepostedPostId set) is what PostCard_Click and the interaction
    // bar's Visibility key off; ViewerRepostId is what RepostedToButtonStyle paints
    // and what the next feed-repost-button tap reverses.

    [Fact]
    public void RepostedPostId_Set_MarksIsRepostRow()
    {
        var repost = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc(), repostedPostId: "orig-1"));
        var ordinary = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc()));

        Assert.Equal("orig-1", repost.RepostedPostId);
        Assert.True(repost.IsRepostRow);
        Assert.Null(ordinary.RepostedPostId);
        Assert.False(ordinary.IsRepostRow);
    }

    [Fact]
    public void ViewerRepostId_BindsRealSnapshotValue()
    {
        var reposted = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc(), viewerRepostId: "my-repost-1"));
        var notReposted = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc()));

        Assert.Equal("my-repost-1", reposted.ViewerRepostId);
        Assert.Null(notReposted.ViewerRepostId);
    }

    /// <summary>The repost toggle's own state must be render-bound content — mirrors
    /// ContentEquals_LikeToggle_False. An unrepost (viewer_repost_id clearing) must
    /// still rebuild the row so the lit RepostedToButtonStyle un-lights.</summary>
    [Fact]
    public void ContentEquals_RepostToggle_False()
    {
        var reposted = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc(), viewerRepostId: "my-repost-1"));
        var notReposted = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc()));
        Assert.False(reposted.ContentEquals(notReposted));
    }

    /// <summary>A row becoming/ceasing a REPOST ROW must repaint: the interaction bar's
    /// Visibility and repost-attribution both key off IsRepostRow.</summary>
    [Fact]
    public void ContentEquals_RepostedPostIdSet_False()
    {
        var ordinary = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc()));
        var repostRow = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc(), repostedPostId: "orig-1"));
        Assert.False(ordinary.ContentEquals(repostRow));
    }

    // ── ContentEquals: the SyncPosts diff-guard key (feed image-render fix) ──
    // Two FeedPostItems built from equal snapshots compare equal, so an UNCHANGED
    // post keeps its row (and its in-flight /api/v1/blob/<hash> load) across observer
    // ticks; any render-relevant change (media/quote/link-preview folded, counts
    // bumped, body edited, remote image revealed) compares unequal so THAT row
    // rebuilds and paints the change (render-model.md § D6). The pre-fix
    // Posts.Clear()+rebuild-all tore down stable rows on every tick.

    [Fact]
    public void ContentEquals_IdenticalSnapshots_True()
    {
        var a = new FeedPostItem(MakeBodyPost(VerificationStatus.Unchecked));
        var b = new FeedPostItem(MakeBodyPost(VerificationStatus.Unchecked));
        Assert.True(a.ContentEquals(b));
    }

    [Fact]
    public void ContentEquals_MediaFold_False()
    {
        // The image-render case: the unresolved post (no Image block) and the resolved
        // post (Image block folded in) compare unequal, so the row rebuilds and paints
        // the post-image. The opposite — treating them equal — is the bug being fixed.
        var unresolved = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("photo") }),
        })));
        var resolved = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("photo") }),
            new RenderBlock.Image("deadbeef", ""),
        })));
        Assert.False(unresolved.ContentEquals(resolved));
    }

    [Fact]
    public void ContentEquals_CountBump_False()
    {
        var before = new FeedPostItem(MakeCountPost(1, 0, 0, 0));
        var after = new FeedPostItem(MakeCountPost(2, 0, 0, 0)); // a like landed
        Assert.False(before.ContentEquals(after));
    }

    /// <summary>The like TOGGLE's own state must be render-bound content, not just its
    /// count: an un-like (count 1 → 0) with viewer_liked flipping true → false must still
    /// rebuild the row even on a snapshot where the count comparison alone wouldn't catch
    /// it (feed.md § Interaction bar → Repost, ratified 2026-08-10).</summary>
    [Fact]
    public void ContentEquals_LikeToggle_False()
    {
        var liked = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc(), viewerLiked: true));
        var unliked = new FeedPostItem(PostSummaryFixture.Make(document: EmptyDoc(), viewerLiked: false));
        Assert.False(liked.ContentEquals(unliked));
    }

    [Fact]
    public void ContentEquals_QuotedPostFold_False()
    {
        var plain = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("look") }),
        })));
        var folded = new FeedPostItem(MakeQuotedPost(VerificationStatus.Unchecked));
        Assert.False(plain.ContentEquals(folded));
    }

    [Fact]
    public void ContentEquals_RemoteImageReveal_False()
    {
        var blocked = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.RemoteImage("https://img.test/c.png", "cat", false),
        })));
        var revealed = new FeedPostItem(MakePost(new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.RemoteImage("https://img.test/c.png", "cat", true),
        })));
        Assert.False(blocked.ContentEquals(revealed));
    }

    // ── Gate-to-tier badge (feed.md § Encryption at rest; monetization.md § Pillars 2+3) ──
    // GatedTier drives the gated-post-badge on the card (tier name); GatedUnlocked flips the
    // detail from teaser to full body. Both are additive PostSummary projections
    // (content_meta.gated_tier) — wire-default null/false when omitted.

    [Fact]
    public void GatedTier_Set_ProjectsBadge()
    {
        var item = new FeedPostItem(MakeGatedPost("gold", unlocked: false));
        Assert.Equal("gold", item.GatedTier);
        Assert.True(item.HasGatedBadge);
        Assert.False(item.GatedUnlocked);
    }

    [Fact]
    public void GatedTier_Null_NoBadge()
    {
        // A public (non-gated) post — null tier means no gated-post-badge.
        var item = new FeedPostItem(MakeGatedPost(null, unlocked: false));
        Assert.Null(item.GatedTier);
        Assert.False(item.HasGatedBadge);
    }

    [Fact]
    public void ContentEquals_GatedUnlock_False()
    {
        // Unlocking a gated post (teaser → full body) is render-bound content: the open
        // detail must repaint, so the two must compare unequal.
        var teaser = new FeedPostItem(MakeGatedPost("gold", unlocked: false));
        var unlocked = new FeedPostItem(MakeGatedPost("gold", unlocked: true));
        Assert.False(teaser.ContentEquals(unlocked));
    }

    [Fact]
    public void ContentEquals_GatedTierBadge_False()
    {
        // A tier badge appearing (public → gated) must repaint the card.
        var publicPost = new FeedPostItem(MakeGatedPost(null, unlocked: false));
        var gated = new FeedPostItem(MakeGatedPost("gold", unlocked: false));
        Assert.False(publicPost.ContentEquals(gated));
    }

    private static PostSummary MakeGatedPost(string? tier, bool unlocked) =>
        PostSummaryFixture.Make(
            body: "teaser", document: EmptyDoc(), gatedTier: tier, gatedUnlocked: unlocked);

    // ── Room-restricted badge (ui/feed.md § Encryption at rest → Room-restricted —
    // the card): a member's card names the room (their own conversation-list label,
    // read fresh against `own_rooms`), never the reserved tier; a reader off the
    // room's floor sees the reserved tier `room` — ruling 3's honest degrade. ──

    /// <summary>Maps the exact templates `i18n/strings/en.yaml` gives these keys — the
    /// two badge tests below check the SUBSTITUTED text, which needs a localizer that
    /// actually holds the keys (the class default otherwise echoes the raw key, as
    /// <see cref="StringsResolutionTests"/> pins).</summary>
    private sealed class GatedBadgeLocalizer : IStringLocalizer
    {
        private static readonly Dictionary<string, string> Map = new()
        {
            ["feed/post/gate_room"] = "Room: {room}",
            ["feed/post/gated_badge_room_tooltip"] = "Room members only: {room}",
            ["feed/post/gated_badge_tooltip"] = "Subscribers only: {tier}",
        };
        public string Get(string key) => Map.TryGetValue(key, out var v) ? v : key;
    }

    [Fact]
    public void RoomLabel_Set_ProjectsRoomBadgeTextAndTooltip()
    {
        Strings.Initialize(new GatedBadgeLocalizer());
        var item = new FeedPostItem(PostSummaryFixture.Make(
            body: "teaser", document: EmptyDoc(),
            gatedTier: "room", gatedRoom: "aa", roomLabel: "Book club"));

        Assert.True(item.HasGatedBadge);
        Assert.Equal("Room: Book club", item.GatedBadgeText);
        Assert.Equal("Room members only: Book club", item.GatedBadgeTooltip);
    }

    [Fact]
    public void RoomLabel_Absent_BadgeFallsBackToReservedTier()
    {
        Strings.Initialize(new GatedBadgeLocalizer());
        var item = new FeedPostItem(PostSummaryFixture.Make(
            body: "teaser", document: EmptyDoc(), gatedTier: "room", gatedRoom: "aa", roomLabel: null));

        Assert.True(item.HasGatedBadge);
        Assert.Equal("room", item.GatedBadgeText);
        Assert.Equal("Subscribers only: room", item.GatedBadgeTooltip);
    }

    [Fact]
    public void ContentEquals_RoomLabelResolves_False()
    {
        // The member's room-membership resolving (locked reserved-tier view -> the
        // room's own label) must repaint the card, exactly as an unlock-offer resolving does.
        var beforeMembershipResolves = new FeedPostItem(PostSummaryFixture.Make(
            body: "teaser", document: EmptyDoc(), gatedTier: "room", gatedRoom: "aa", roomLabel: null));
        var afterMembershipResolves = new FeedPostItem(PostSummaryFixture.Make(
            body: "teaser", document: EmptyDoc(), gatedTier: "room", gatedRoom: "aa", roomLabel: "Book club"));
        Assert.False(beforeMembershipResolves.ContentEquals(afterMembershipResolves));
    }

    // ── Sold-post buyer teaser (gap (2c), monetization.md § Per-post pay-to-unlock) ──
    // unlockOffer null covers both "not yet resolved" and "the nest answered no offer" — both
    // leave the priceless teaser (no gated-post-price/-payment-link/-buy-button).

    [Fact]
    public void UnlockOffer_Absent_NoTeaser()
    {
        var item = new FeedPostItem(MakeGatedPost("post-unlock-abc", unlocked: false));
        Assert.False(item.HasUnlockOffer);
        Assert.Equal("", item.UnlockOfferPriceText);
        Assert.False(item.HasUnlockPaymentLink);
    }

    [Fact]
    public void UnlockOffer_Resolved_ProjectsPriceAndPaymentLink()
    {
        var item = new FeedPostItem(PostSummaryFixture.Make(
            body: "teaser", document: EmptyDoc(), gatedTier: "post-unlock-abc",
            unlockOffer: new UnlockOfferView("post-unlock-abc", "$3", "https://pay.example/x")));

        Assert.True(item.HasUnlockOffer);
        Assert.Equal("$3", item.UnlockOfferPriceText);
        Assert.True(item.HasUnlockPaymentLink);
        Assert.Equal("https://pay.example/x", item.UnlockOfferPaymentUrl);
    }

    [Fact]
    public void UnlockOffer_ResolvedNoPaymentUrl_HasNoPaymentLink()
    {
        // A claim-code-only seller has no external checkout — the payment-link button
        // stays hidden, but the price + buy button still render.
        var item = new FeedPostItem(PostSummaryFixture.Make(
            body: "teaser", document: EmptyDoc(), gatedTier: "post-unlock-abc",
            unlockOffer: new UnlockOfferView("post-unlock-abc", "$3", null)));

        Assert.True(item.HasUnlockOffer);
        Assert.False(item.HasUnlockPaymentLink);
    }

    [Fact]
    public void ContentEquals_UnlockOfferResolves_False()
    {
        // The price-resolve trigger's whole point: an unresolved → resolved transition must
        // repaint the row so the teaser price/buy-button appear.
        var unresolved = new FeedPostItem(MakeGatedPost("post-unlock-abc", unlocked: false));
        var resolved = new FeedPostItem(PostSummaryFixture.Make(
            body: "teaser", document: EmptyDoc(), gatedTier: "post-unlock-abc",
            unlockOffer: new UnlockOfferView("post-unlock-abc", "$3", null)));
        Assert.False(unresolved.ContentEquals(resolved));
    }

    private static PostSummary MakeCountPost(long like, long reply, long repost, long quote) =>
        PostSummaryFixture.Make(
            document: EmptyDoc(),
            likeCount: like, replyCount: reply, repostCount: repost, quoteCount: quote);

    private static RenderDocument EmptyDoc() =>
        new RenderDocument(new RenderBlock[]
        {
            new RenderBlock.Paragraph(new Inline[] { new Inline.Text("body") }),
        });

    // ── C2PA provenance badge (media.md § C2PA provenance) — HasC2pa is page-side async state (FeedPage.SyncPosts sets it via
    // SetHasC2pa after BlobImageLoader.LoadWithC2paAsync resolves), not snapshot
    // content, which is exactly why it is unit-testable in isolation from any XAML
    // control or driver. ──

    [Fact]
    public void HasC2pa_DefaultsFalseAndUnchecked()
    {
        var item = new FeedPostItem(MakePost(EmptyDoc()));

        Assert.False(item.HasC2pa);
        Assert.False(item.C2paChecked);
    }

    [Fact]
    public void SetHasC2pa_True_SetsBothFlagsAndRaisesPropertyChanged()
    {
        var item = new FeedPostItem(MakePost(EmptyDoc()));
        var raised = false;
        item.PropertyChanged += (_, e) => raised |= e.PropertyName == nameof(FeedPostItem.HasC2pa);

        item.SetHasC2pa(true);

        Assert.True(item.HasC2pa);
        Assert.True(item.C2paChecked);
        Assert.True(raised);
    }

    [Fact]
    public void SetHasC2pa_False_MarksCheckedWithoutRaising()
    {
        // The negative result (a plain, unsigned image) is the common case — must not
        // spuriously notify, and must still flip the fire-once guard so
        // FeedPage.SyncPosts's trigger does not refire it every observer tick.
        var item = new FeedPostItem(MakePost(EmptyDoc()));
        var raised = false;
        item.PropertyChanged += (_, e) => raised |= e.PropertyName == nameof(FeedPostItem.HasC2pa);

        item.SetHasC2pa(false);

        Assert.False(item.HasC2pa);
        Assert.True(item.C2paChecked);
        Assert.False(raised);
    }

    [Fact]
    public void ContentEquals_IgnoresHasC2pa()
    {
        // HasC2pa is deliberately excluded from ContentEquals (FeedViewModel.cs's own
        // doc comment on the property): it is page-side async state, not snapshot
        // content, and ObservableCollectionReconcile keeping the existing instance
        // across a content-equal tick is exactly what lets the fire-once check survive
        // un-repeated. If a snapshot-equal item were ever unequal because of this
        // field, Reconcile would replace the row on every tick and the check would
        // refire (and re-request) forever.
        var checkedTrue = new FeedPostItem(MakePost(EmptyDoc()));
        checkedTrue.SetHasC2pa(true);
        var uncheckedItem = new FeedPostItem(MakePost(EmptyDoc()));

        Assert.True(checkedTrue.ContentEquals(uncheckedItem));
    }
}
