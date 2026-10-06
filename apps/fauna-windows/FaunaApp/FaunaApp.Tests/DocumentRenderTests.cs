using System.Linq;
using FaunaApp.Core.Helpers;
using uniffi.fauna_core;
using Xunit;

namespace FaunaApp.Tests;

/// <summary>
/// Unit coverage for <see cref="DocumentRenderer.Flatten"/>: windows walks the shared
/// <c>RenderDocument</c> (<c>MessageSnapshot.document</c>, produced once by the
/// conversations manager — render-model.md § D1) into a flat <see cref="MarkdownRun"/>
/// list for the DM bubble, instead of re-parsing <c>msg.body</c> with the markdown
/// parser at render time. Mirrors the linux <c>document.rs</c> walker and the android
/// <c>DocumentText.kt</c> walker (priority #1/#3 — one shape on every app). The
/// producer is shared Rust and not UniFFI-exported, so — like the android
/// <c>DocumentTextTest</c> — these construct <c>RenderDocument</c> records directly and
/// assert the walk, no native call.
/// </summary>
public class DocumentRenderTests
{
    private const string Bullet = "• "; // "• "

    // ── Builders (terse record construction; Vec<T> ⇒ T[] in the C# binding) ──
    private static RenderDocument Doc(params RenderBlock[] blocks) => new(blocks);
    private static Inline T(string s) => new Inline.Text(s);
    private static Inline B(params Inline[] i) => new Inline.Bold(i);
    private static Inline It(params Inline[] i) => new Inline.Italic(i);
    private static Inline Code(string s) => new Inline.Code(s);
    private static Inline Link(string href, params Inline[] i) => new Inline.Link(href, i);
    private static RenderBlock P(params Inline[] i) => new RenderBlock.Paragraph(i);
    private static RenderBlock H(byte level, params Inline[] i) => new RenderBlock.Heading(level, i);
    // Attachment ctor order mirrors the Rust field order: blob_hash, filename, mime_type,
    // size_bytes, is_image, c2pa (u64 ⇒ ulong).
    private static RenderBlock Att(string blobHash, string filename, bool isImage,
        bool c2pa = false, ulong sizeBytes = 0, string mimeType = "application/octet-stream") =>
        new RenderBlock.Attachment(blobHash, filename, mimeType, sizeBytes, isImage, c2pa);
    // Slice 2b added `verification` (defaulted Unchecked so existing callers are
    // unchanged); the windows badge render leg passes Failed to
    // assert the quoted-post `unverified-source-badge`. Delegates to the shared
    // RenderBlockFixture so this type's field list lives in exactly one file.
    private static RenderBlock Quote(string postId, string author, string body,
        VerificationStatus verification = VerificationStatus.Unchecked,
        string? legalTakedownRef = null) =>
        RenderBlockFixture.QuotedPost(postId, author, body, verification,
            legalTakedownRef: legalTakedownRef);
    private static RenderBlock Img(string hash, string alt = "") => new RenderBlock.Image(hash, alt);
    private static RenderBlock Vid(string hash, string alt = "") => new RenderBlock.Video(hash, alt);
    // QuotedMessage ctor order mirrors the Rust field order: author_display, snippet.
    private static RenderBlock QMsg(string author, string snippet) =>
        new RenderBlock.QuotedMessage(author, snippet);
    // LinkPreview ctor order mirrors the Rust field order: url, state (render-model.md § D4).
    private static RenderBlock LinkPreview(string url, PreviewState state) =>
        new RenderBlock.LinkPreview(url, state);
    // TaskList (render-model.md § D7a): each item carries a checked state + a sub-document
    // (one Paragraph from markdown today). TaskItem ctor order mirrors the Rust field
    // order: checked, blocks (Vec<RenderBlock> ⇒ RenderBlock[]).
    private static RenderBlock Tasks(params (bool Checked, string Text)[] items) =>
        new RenderBlock.TaskList(items
            .Select(i => new TaskItem(i.Checked, new[] { P(T(i.Text)) }))
            .ToArray());

    [Fact]
    public void Flatten_Paragraph_CarriesInlineBoldItalicCode()
    {
        var runs = DocumentRenderer.Flatten(Doc(P(
            T("Plain "), B(T("bold")), T(" and "), It(T("italic")), T(" and "), Code("code"))));

        Assert.Contains(runs, r => r.Text == "bold" && r.Bold && !r.Italic && !r.Monospace);
        Assert.Contains(runs, r => r.Text == "italic" && r.Italic && !r.Bold);
        Assert.Contains(runs, r => r.Text == "code" && r.Monospace && !r.Bold);
    }

    [Fact]
    public void Flatten_Link_ProducesHrefBearingRun()
    {
        var runs = DocumentRenderer.Flatten(Doc(P(
            T("see "), Link("https://x.io", T("label")), T(" now"))));

        var link = Assert.Single(runs, r => !string.IsNullOrEmpty(r.Link));
        Assert.Equal("label", link.Text);
        Assert.Equal("https://x.io", link.Link);
    }

    [Fact]
    public void Flatten_Heading_CarriesLevel()
    {
        var runs = DocumentRenderer.Flatten(Doc(H(2, T("Heading "), B(T("b")))));

        Assert.Contains(runs, r => r.Text.Contains("Heading") && r.HeadingLevel == 2);
        // Inline emphasis inside a heading is preserved alongside the heading level.
        Assert.Contains(runs, r => r.Text == "b" && r.HeadingLevel == 2 && r.Bold);
    }

    [Fact]
    public void Flatten_List_EmitsBulletsAndItemBreaks()
    {
        var runs = DocumentRenderer.Flatten(Doc(new RenderBlock.ListBlock(
            false, new[] { Doc(P(T("one"))), Doc(P(T("two"))) })));

        Assert.Equal(2, runs.Count(r => r.Text == Bullet));
        Assert.Contains(runs, r => r.Text == "one");
        Assert.Contains(runs, r => r.Text == "two");
        // Two items ⇒ a hard break separating them.
        Assert.Contains(runs, r => r.IsLineBreak);
    }

    [Fact]
    public void Flatten_TaskList_EmitsCheckboxGlyphsAndItemText()
    {
        // render-model.md § D7a: a `- [ ]`/`- [x]` run folds into a TaskList; the read-side
        // walker prefixes each item's content with a static ☐/☑ glyph per its checked state.
        var runs = DocumentRenderer.Flatten(Doc(Tasks((false, "todo"), (true, "done"))));

        // The glyph is followed by a NO-BREAK SPACE (U+00A0), so the checkbox stays
        // attached to its item text instead of wrapping away from it.
        Assert.Contains(runs, r => r.Text == "☐ ");
        Assert.Contains(runs, r => r.Text == "☑ ");
        Assert.Contains(runs, r => r.Text == "todo");
        Assert.Contains(runs, r => r.Text == "done");
        // Two items ⇒ a hard break separating them.
        Assert.Contains(runs, r => r.IsLineBreak);
    }

    [Fact]
    public void Flatten_OrderedList_EmitsNumberedPrefixes()
    {
        var runs = DocumentRenderer.Flatten(Doc(new RenderBlock.ListBlock(
            true, new[] { Doc(P(T("one"))), Doc(P(T("two"))), Doc(P(T("ten"))) })));

        // Renumbered from 1 (the shared model carries no item numbers).
        Assert.Contains(runs, r => r.Text == "1. ");
        Assert.Contains(runs, r => r.Text == "2. ");
        Assert.Contains(runs, r => r.Text == "3. ");
        Assert.Contains(runs, r => r.Text == "ten");
    }

    [Fact]
    public void Flatten_Blockquote_IsItalic()
    {
        var runs = DocumentRenderer.Flatten(Doc(
            new RenderBlock.BlockQuote(new[] { P(T("quoted text")) })));

        Assert.Contains(runs, r => r.Text == "quoted text" && r.Italic);
    }

    [Fact]
    public void Flatten_BoldItalic_SetsBothEmphases()
    {
        // The producer nests `***x***` as Bold(Italic(Text)); the walk accumulates both.
        var runs = DocumentRenderer.Flatten(Doc(P(B(It(T("both"))))));

        Assert.Contains(runs, r => r.Text == "both" && r.Bold && r.Italic);
    }

    [Fact]
    public void Flatten_FencedCode_IsMonospace()
    {
        var runs = DocumentRenderer.Flatten(Doc(new RenderBlock.CodeBlock(null, "let x = 1;\n")));

        Assert.Contains(runs, r => r.Monospace && r.Text.Contains("let x = 1;"));
    }

    [Fact]
    public void Flatten_MultipleBlocks_SeparatedByLineBreak()
    {
        var runs = DocumentRenderer.Flatten(Doc(H(1, T("Title")), P(T("body text"))));

        Assert.Contains(runs, r => r.Text == "Title" && r.HeadingLevel == 1);
        Assert.Contains(runs, r => r.Text == "body text" && r.HeadingLevel == 0);
        Assert.Contains(runs, r => r.IsLineBreak);
    }

    [Fact]
    public void Flatten_PlainText_IsASingleRun()
    {
        var runs = DocumentRenderer.Flatten(Doc(P(T("just words"))));

        var run = Assert.Single(runs);
        Assert.Equal("just words", run.Text);
        Assert.False(run.Bold || run.Italic || run.Monospace || run.IsLineBreak);
        Assert.Equal(0, run.HeadingLevel);
        Assert.Null(run.Link);
    }

    // ── D3/D5: body remote images are their own `doc-remote-image` element ──
    // render-model.md § D3 + § Implementation status; apps/tui.md § Rendering. A body
    // RenderBlock::RemoteImage USED TO paint inline as a placeholder text run (html-mail
    // Slice 3); since ui.yaml minted `doc-remote-image` (indexed, user-approved
    // 2026-07-31) it obeys the same rule as Image/Attachment/QuotedPost/QuotedMessage —
    // "a block with its own element ID is painted by the PAGE, as that element" — so the
    // walker arm is inert (like its four siblings) and DocumentRenderer.RemoteImages
    // extracts the list for the page to paint as its own widgets.

    [Fact]
    public void Flatten_RemoteImage_IsInert_PaintedAsItsOwnElementNotBodyText()
    {
        // Mirrors Flatten_SkipsQuotedPostAndImageBlocks_PaintedInTheirOwnWidgets /
        // Flatten_SkipsAttachmentBlocks_PaintedInTheirOwnPanel: a RemoteImage block
        // contributes NOTHING to the body walk (no run, no stray line break) — the page
        // paints it separately via RemoteImages(doc), just under the body.
        var runs = DocumentRenderer.Flatten(Doc(
            P(T("body")),
            new RenderBlock.RemoteImage("https://img.test/c.png", "a grey cat", false)));

        var run = Assert.Single(runs);
        Assert.Equal("body", run.Text);
        Assert.False(run.IsLineBreak);
    }

    [Fact]
    public void RemoteImages_ExtractsRemoteImageBlocksInBodyOrder()
    {
        var images = DocumentRenderer.RemoteImages(Doc(
            P(T("see ")),
            new RenderBlock.RemoteImage("https://img.test/a.png", "first", false),
            new RenderBlock.RemoteImage("https://img.test/b.png", "second", true)));

        Assert.Equal(2, images.Count);
        Assert.Equal("https://img.test/a.png", images[0].url);
        Assert.Equal("first", images[0].alt);
        Assert.False(images[0].revealed);
        Assert.Equal("https://img.test/b.png", images[1].url);
        Assert.Equal("second", images[1].alt);
        Assert.True(images[1].revealed);
    }

    [Fact]
    public void RemoteImages_EmptyWhenNoRemoteImageBlocks()
    {
        Assert.Empty(DocumentRenderer.RemoteImages(Doc(P(T("body only")))));
    }

    /// <summary>Single-sourced on the shared Rust <c>RenderDocument::remote_images</c> via
    /// <c>render_document_remote_images</c> — recurses, unlike a hand-rolled top-level-only
    /// walk would (the same per-app omission vector <c>QuotedPost</c>/<c>MediaImageHash</c>
    /// already closed).</summary>
    [Fact]
    public void RemoteImages_IsFoundWhenNestedInABlockQuote()
    {
        var images = DocumentRenderer.RemoteImages(Doc(
            P(T("quoting:")),
            new RenderBlock.BlockQuote(new RenderBlock[]
            {
                new RenderBlock.RemoteImage("https://img.test/nested.png", "nested", false),
            })));

        var image = Assert.Single(images);
        Assert.Equal("https://img.test/nested.png", image.url);
    }

    /// <summary>D3: <c>HasBlockedRemoteImage</c> is true when ≥1 <c>RemoteImage</c>
    /// block has <c>revealed==false</c> (manager default); false when all remote images are
    /// already revealed (manager projected <c>revealed=true</c>) or none are present.
    /// This is the gate for the <c>load-remote-content-button</c> after D3
    /// (render-model.md § D3; mirrors the shared Rust <c>has_blocked_remote_images</c>).</summary>
    [Fact]
    public void HasBlockedRemoteImage_TrueWhenBlockedFalseWhenRevealedOrAbsent()
    {
        // Blocked (revealed=false): manager has not revealed this image yet.
        var blocked = Doc(
            P(T("see ")),
            new RenderBlock.RemoteImage("https://img.test/c.png", "a cat", false));
        Assert.True(DocumentRenderer.HasBlockedRemoteImage(blocked));

        // Revealed (revealed=true): manager flipped the reveal set + re-emitted.
        var revealed = Doc(
            P(T("see ")),
            new RenderBlock.RemoteImage("https://img.test/c.png", "a cat", true));
        Assert.False(DocumentRenderer.HasBlockedRemoteImage(revealed));

        // No remote image at all: button must not appear.
        var noImage = Doc(P(B(T("bold")), T(" and a "), Link("https://x.io", T("link"))));
        Assert.False(DocumentRenderer.HasBlockedRemoteImage(noImage));

        // Mixed: one blocked + one revealed → still has a blocked image.
        var mixed = Doc(
            new RenderBlock.RemoteImage("https://img.test/a.png", "first", false),
            new RenderBlock.RemoteImage("https://img.test/b.png", "second", true));
        Assert.True(DocumentRenderer.HasBlockedRemoteImage(mixed));
    }

    /// <summary>D4 og:image reveal gate (render-model.md § D4, user-ratified 2026-06-27):
    /// a Resolved link-preview whose og:image (<c>image_hash</c>) is not yet revealed is a
    /// blocked remote image too — so the post's ONE <c>load-remote-content-button</c> reveals
    /// body images AND the og:image together. Mirrors the shared
    /// <c>has_blocked_remote_images</c> LinkPreview arm (render.rs). A Resolved-with-no-image /
    /// Resolving / Failed preview arms nothing (no remote content to block).</summary>
    [Fact]
    public void HasBlockedRemoteImage_TrueForAnUnrevealedLinkPreviewOgImage()
    {
        // Resolved + og:image present + blocked (revealed=false): the button shows even though
        // the body carries no remote image — only the kept inline link.
        var blocked = Doc(
            P(Link("https://example.com/a", T("https://example.com/a"))),
            LinkPreview("https://example.com/a",
                new PreviewState.Resolved("T", "D", "imghash", revealed: false)));
        Assert.True(DocumentRenderer.HasBlockedRemoteImage(blocked));

        // Revealed og:image: the manager flipped revealed → no longer blocked.
        var revealed = Doc(
            P(Link("https://example.com/a", T("https://example.com/a"))),
            LinkPreview("https://example.com/a",
                new PreviewState.Resolved("T", "D", "imghash", revealed: true)));
        Assert.False(DocumentRenderer.HasBlockedRemoteImage(revealed));

        // Resolved but NO og:image (image_hash null): nothing to block even when un-revealed.
        var noImage = Doc(
            P(Link("https://example.com/a", T("https://example.com/a"))),
            LinkPreview("https://example.com/a",
                new PreviewState.Resolved("T", "D", null, revealed: false)));
        Assert.False(DocumentRenderer.HasBlockedRemoteImage(noImage));

        // Resolving / Failed: no resolved og:image, nothing to block.
        Assert.False(DocumentRenderer.HasBlockedRemoteImage(Doc(
            P(T("x")), LinkPreview("https://example.com/a", new PreviewState.Resolving()))));
        Assert.False(DocumentRenderer.HasBlockedRemoteImage(Doc(
            P(T("x")), LinkPreview("https://example.com/a", new PreviewState.Failed()))));
    }

    /// <summary>Drift-fix (2026-07-02): the reveal-gate is now single-sourced on the shared Rust
    /// <c>RenderDocument::has_blocked_remote_images</c> via the UniFFI face
    /// (<c>render_document_has_blocked_remote_images</c>), which recurses into lists/quotes at ANY
    /// depth. A Resolved link-preview og:image nested inside a <c>BlockQuote</c> is therefore blocked
    /// remote content too — the old windows top-level-only <c>HasBlockedLinkPreviewImage</c> fold
    /// returned <c>false</c> for exactly this shape. Runs over the real native FFI (the dll loads),
    /// so it doubles as a shared-vs-windows conformance check.</summary>
    [Fact]
    public void HasBlockedRemoteImage_TrueForANestedLinkPreviewOgImage()
    {
        // A blocked Resolved og:image buried inside a block quote. The producer folds link previews
        // at top level today, but single-sourcing the walk means windows can never silently miss a
        // nested embed the way the old top-level-only fold did.
        var blockedNested = Doc(
            P(T("quoting a link:")),
            new RenderBlock.BlockQuote(new[]
            {
                LinkPreview("https://example.com/a",
                    new PreviewState.Resolved("T", "D", "imghash", revealed: false)),
            }));
        Assert.True(DocumentRenderer.HasBlockedRemoteImage(blockedNested));

        // Revealed nested og:image → not blocked (the shared walk honors the projected flag).
        var revealedNested = Doc(
            new RenderBlock.BlockQuote(new[]
            {
                LinkPreview("https://example.com/a",
                    new PreviewState.Resolved("T", "D", "imghash", revealed: true)),
            }));
        Assert.False(DocumentRenderer.HasBlockedRemoteImage(revealedNested));
    }

    // ── D2: attachments are first-class blocks (render-model.md § D2) ──
    // The conversations manager folds one Attachment block per MessageSnapshot.attachments
    // entry into the document after the body; the windows bubble paints them as rich
    // dm-attachment-image / dm-attachment-file widgets in its AttachmentsList panel, sourced
    // from the document (no sibling field). Mirrors linux document.rs's in-walk Attachment arm.

    [Fact]
    public void Attachments_ExtractsAttachmentBlocksInBodyOrder()
    {
        var doc = Doc(
            P(T("here are the files")),
            Att("hashA", "cat.png", isImage: true, c2pa: true, sizeBytes: 2048),
            Att("hashB", "notes.txt", isImage: false, sizeBytes: 12));

        var atts = DocumentRenderer.Attachments(doc);

        Assert.Equal(2, atts.Count);
        Assert.Equal("hashA", atts[0].blobHash);
        Assert.Equal("cat.png", atts[0].filename);
        Assert.True(atts[0].isImage);
        Assert.True(atts[0].c2pa);
        Assert.Equal("notes.txt", atts[1].filename);
        Assert.False(atts[1].isImage);
        Assert.Equal(12u, atts[1].sizeBytes);
    }

    [Fact]
    public void Attachments_EmptyWhenNoAttachmentBlocks()
    {
        Assert.Empty(DocumentRenderer.Attachments(Doc(P(T("body only")))));
    }

    // ── D6 embed-fold: quoted-post + media folded into the feed document ──
    // The feed manager folds a QuotedPost block (resolve_quoted_post) and an Image block
    // (resolve_media) into PostSummary.document AFTER the body. The feed list card + detail
    // paint these as the quoted-post Border + post-image Image widgets, so — like an
    // Attachment — they are EXTRACTED here and EXCLUDED from the body flatten (no trailing
    // blank line). Mirrors linux document.rs's quoted_post / media_image extractors.

    [Fact]
    public void QuotedPost_ExtractsTheFoldedQuotedPostBlock()
    {
        var block = DocumentRenderer.QuotedPost(Doc(
            P(T("nice post")),
            Quote("post99", "ffeeaa", "the quoted body")));

        Assert.NotNull(block);
        Assert.Equal("post99", block!.postId);
        Assert.Equal("ffeeaa", block.author);
        Assert.Equal("the quoted body", block.body);
    }

    [Fact]
    public void QuotedPost_NullWhenNoQuotedPostBlock()
    {
        Assert.Null(DocumentRenderer.QuotedPost(Doc(P(T("body only")))));
    }

    [Fact]
    public void MediaImageHash_ExtractsTheFoldedImageBlockHash()
    {
        Assert.Equal("abc123", DocumentRenderer.MediaImageHash(Doc(
            P(T("with media")),
            Img("abc123"))));
    }

    [Fact]
    public void MediaImageHash_NullWhenNoImageBlock()
    {
        Assert.Null(DocumentRenderer.MediaImageHash(Doc(P(T("body only")))));
    }

    // ── The D6b typed Video sibling of Image — the exact twin of the two tests above ──

    [Fact]
    public void MediaVideoHash_ExtractsTheFoldedVideoBlockHash()
    {
        Assert.Equal("feedbeef", DocumentRenderer.MediaVideoHash(Doc(
            P(T("with video")),
            Vid("feedbeef"))));
    }

    [Fact]
    public void MediaVideoHash_NullWhenNoVideoBlock()
    {
        Assert.Null(DocumentRenderer.MediaVideoHash(Doc(P(T("body only")))));
    }

    // ── The recursion the shared faces buy, invisible to every extractor test above ──
    // Both delegations replaced a top-level-only `doc.blocks.OfType<…>().FirstOrDefault()`
    // twin. These two FAIL against those twins, which is the point: a lift whose payoff no
    // test can see is unproven (the property apple pinned + mutation-proved in
    // DocumentEmbedExtractorTests; render-model.md § Implementation status).

    [Fact]
    public void QuotedPost_IsFoundWhenNestedInABlockQuote()
    {
        var block = DocumentRenderer.QuotedPost(Doc(
            P(T("nice post")),
            new RenderBlock.BlockQuote(new[]
            {
                Quote("post99", "ffeeaa", "the quoted body"),
            })));

        Assert.NotNull(block);
        Assert.Equal("post99", block!.postId);
    }

    [Fact]
    public void MediaImageHash_IsFoundWhenNestedInABlockQuote()
    {
        Assert.Equal("abc123", DocumentRenderer.MediaImageHash(Doc(
            P(T("with media")),
            new RenderBlock.BlockQuote(new[] { Img("abc123") }))));
    }

    [Fact]
    public void MediaVideoHash_IsFoundWhenNestedInABlockQuote()
    {
        Assert.Equal("feedbeef", DocumentRenderer.MediaVideoHash(Doc(
            P(T("with video")),
            new RenderBlock.BlockQuote(new[] { Vid("feedbeef") }))));
    }

    [Fact]
    public void Flatten_SkipsQuotedPostAndImageBlocks_PaintedInTheirOwnWidgets()
    {
        // The quoted-post + media fold into the document after the body, but the feed paints
        // them as the quoted-post Border + post-image Image — NOT body text. So the body
        // flatten must drop them and not leave a trailing blank line for each (same
        // exclusion as Attachment).
        var runs = DocumentRenderer.Flatten(Doc(
            P(T("body")),
            Quote("p", "a", "quoted"),
            Img("hash")));

        var run = Assert.Single(runs);
        Assert.Equal("body", run.Text);
        Assert.False(run.IsLineBreak);
    }

    [Fact]
    public void Flatten_SkipsVideoBlock_PaintedInItsOwnWidget()
    {
        // The D6b typed sibling of Image — same exclusion, same reason: video-thumbnail
        // paints from MediaVideoHash, not body text.
        var runs = DocumentRenderer.Flatten(Doc(P(T("body")), Vid("hash")));

        var run = Assert.Single(runs);
        Assert.Equal("body", run.Text);
        Assert.False(run.IsLineBreak);
    }

    [Fact]
    public void Flatten_SkipsAttachmentBlocks_PaintedInTheirOwnPanel()
    {
        // Attachments paint as rich widgets in AttachmentsList, NOT as body text — so the body
        // flatten must not surface the filename/hash into dm-message-text, nor leave a trailing
        // blank line for each (the inter-block break is suppressed by excluding them).
        var runs = DocumentRenderer.Flatten(Doc(
            P(T("body")),
            Att("hashA", "cat.png", isImage: true),
            Att("hashB", "notes.txt", isImage: false)));

        var run = Assert.Single(runs);
        Assert.Equal("body", run.Text);
        Assert.False(run.IsLineBreak);
    }

    // ── D2b in-bubble reply-quote: QuotedMessage folded into a reply's document ──
    // The conversations manager folds a RenderBlock.QuotedMessage{author_display, snippet}
    // into a *reply* message's document at read time (thread_detail's fold_reply_quotes),
    // PREPENDED so the in-order walkers paint it above the body (render-model.md § D2). The
    // windows bubble paints it as the dm-message-quote card from this extractor, so — like a
    // QuotedPost / Attachment — it is EXTRACTED here and EXCLUDED from the body flatten (no
    // trailing blank line, no quote text leaking into dm-message-text). Mirrors the linux
    // build_reply_quote_card / android documentQuotedMessage / web quotedMessageBlock.

    [Fact]
    public void QuotedMessage_ExtractsTheFoldedQuotedMessageBlock()
    {
        var block = DocumentRenderer.QuotedMessage(Doc(
            P(T("reply body")),
            QMsg("Carol", "the parent text")));

        Assert.NotNull(block);
        Assert.Equal("Carol", block!.authorDisplay);
        Assert.Equal("the parent text", block.snippet);
    }

    [Fact]
    public void QuotedMessage_NullWhenNoQuotedMessageBlock()
    {
        Assert.Null(DocumentRenderer.QuotedMessage(Doc(P(T("body only")))));
    }

    [Fact]
    public void Flatten_SkipsQuotedMessageBlock_PaintedAsTheReplyQuoteCard()
    {
        // The reply-quote folds into the document, but the bubble paints it as the
        // dm-message-quote card — NOT body text. So the body flatten must drop it and not
        // leave a trailing blank line / leak the author/snippet into dm-message-text (same
        // exclusion as QuotedPost / Attachment).
        var runs = DocumentRenderer.Flatten(Doc(
            P(T("reply body")),
            QMsg("Carol", "the parent text")));

        var run = Assert.Single(runs);
        Assert.Equal("reply body", run.Text);
        Assert.False(run.IsLineBreak);
        Assert.DoesNotContain(runs, r => r.Text.Contains("Carol"));
        Assert.DoesNotContain(runs, r => r.Text.Contains("parent text"));
    }

    // ── Segments: the body as paint units (render-model.md § Implementation status today,
    // the windows line-run entry). One `TextBlock` per segment when a block exceeds the
    // shared `MAX_LINES_PER_TEXT_RUN` budget, so a multi-megabyte plain-text mail is
    // virtualized instead of laid out as one widget. The split itself is shared Rust
    // (`render_inline_line_runs` / `render_text_line_runs`, called through the REAL
    // fauna_ffi dll here — memory reference_windows_dotnet_test_loads_native_ffi).

    private static string SegmentText(IReadOnlyList<MarkdownRun> segment) =>
        string.Concat(segment.Select(r => r.IsLineBreak ? "\n" : r.Text));

    private static string Lines(int n, string prefix = "line") =>
        string.Join("\n", Enumerable.Range(1, n).Select(i => $"{prefix} {i}"));

    [Fact]
    public void Segments_WithinBudget_OneSegmentPerBodyBlock_JoinsBackToFlatten()
    {
        var doc = Doc(P(T("first")), H(2, T("head")), P(T("last")));

        var paint = DocumentRenderer.Segments(doc);

        Assert.False(paint.Split);
        Assert.Equal(3, paint.Segments.Count);
        // No block over budget ⇒ the one-widget paint is byte-identical to Flatten: the
        // segments joined by the inter-block hard break ARE the flattened run list.
        Assert.Equal(DocumentRenderer.Flatten(doc), paint.Joined().ToList());
    }

    [Fact]
    public void Segments_ExcludesEmbedBlocks_LikeFlatten()
    {
        var paint = DocumentRenderer.Segments(Doc(
            QMsg("Carol", "the parent text"),
            P(T("body")),
            Att("hash", "a.png", isImage: true)));

        Assert.False(paint.Split);
        var only = Assert.Single(paint.Segments);
        Assert.Equal("body", SegmentText(only));
    }

    [Fact]
    public void Segments_LongParagraph_SplitsAtTheSharedBudget_Lossless()
    {
        // 40 hard-broken lines in ONE paragraph — the plain-text mail shape
        // (`plaintext_to_document` keeps a blank-line-free body as a single paragraph).
        var text = Lines(40);
        var paint = DocumentRenderer.Segments(Doc(P(T("intro ")), P(T(text)), P(T("outro"))));

        Assert.True(paint.Split);
        // intro + the split paragraph's runs + outro, in body order.
        Assert.True(paint.Segments.Count > 3, $"expected a split, got {paint.Segments.Count} segments");
        Assert.Equal("intro ", SegmentText(paint.Segments[0]));
        Assert.Equal("outro", SegmentText(paint.Segments[^1]));
        var middle = paint.Segments.Skip(1).Take(paint.Segments.Count - 2).ToList();
        // Lossless: each cut consumed exactly the one `\n` it fell on, so the runs'
        // text joined with `\n` gives back the paragraph.
        Assert.Equal(text, string.Join("\n", middle.Select(SegmentText)));
        // No run carries more lines than the shared budget (16 lines ⇒ ≤ 15 breaks).
        Assert.All(middle, s => Assert.True(SegmentText(s).Count(c => c == '\n') <= 15));
    }

    [Fact]
    public void Segments_LongCodeBlock_SplitsMonospace_Lossless()
    {
        var code = Lines(40, "let x =") + "\n";
        var paint = DocumentRenderer.Segments(Doc(new RenderBlock.CodeBlock(null, code)));

        Assert.True(paint.Split);
        Assert.True(paint.Segments.Count > 1);
        Assert.All(paint.Segments, s => Assert.All(s, r => Assert.True(r.IsLineBreak || r.Monospace)));
        Assert.Equal(code.TrimEnd('\n'), string.Join("\n", paint.Segments.Select(SegmentText)));
    }
}
