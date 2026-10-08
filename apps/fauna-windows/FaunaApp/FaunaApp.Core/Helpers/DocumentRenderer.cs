using System.Collections.Generic;
using System.Linq;
using uniffi.fauna_core;

namespace FaunaApp.Core.Helpers;

/// <summary>
/// One renderable piece of a walked <c>RenderDocument</c> body: a styled text run
/// (optionally a <c>[label](url)</c> link), a remote-image placeholder, or a hard line
/// break. Produced by <see cref="DocumentRenderer.Flatten"/>; the shared
/// <c>FaunaApp.Helpers.DocumentPainter.Apply</c> maps each run to a WinUI <c>Inline</c>
/// (the conversations bubble AND the feed post card/detail). The body
/// structure is decided once in shared Rust (<c>fauna_core::render</c>) and never
/// re-derived per client (priority #1/#2/#4).
/// </summary>
public record MarkdownRun(
    string Text,
    bool Bold = false,
    bool Italic = false,
    bool Monospace = false,
    string? Link = null,
    int HeadingLevel = 0, // 1–4 ⇒ heading text (larger/bold at render); 0 otherwise
    bool IsLineBreak = false);

/// <summary>
/// A body walked into paint units by <see cref="DocumentRenderer.Segments"/>: one run list per
/// segment, in body order, and whether any block was split at the shared line-run budget.
/// <see cref="Split"/> false ⇒ <see cref="Joined"/> is exactly <see cref="DocumentRenderer.Flatten"/>.
/// </summary>
public sealed record BodyPaint(IReadOnlyList<IReadOnlyList<MarkdownRun>> Segments, bool Split)
{
    /// <summary>The segments as one run list, the inter-block hard break between them — the
    /// one-widget paint (<see cref="DocumentRenderer.Flatten"/>'s shape).</summary>
    public IEnumerable<MarkdownRun> Joined()
    {
        for (int i = 0; i < Segments.Count; i++)
        {
            if (i > 0)
                yield return new MarkdownRun(string.Empty, IsLineBreak: true);
            foreach (var run in Segments[i])
                yield return run;
        }
    }
}

/// <summary>
/// Walks the shared semantic <c>RenderDocument</c> (<c>MessageSnapshot.document</c>,
/// produced once by the conversations manager via <c>document_for_body</c> —
/// <c>docs/goal/architecture/render-model.md</c> § D1) into a flat
/// <see cref="MarkdownRun"/> list for the DM message bubble. No client re-parses or
/// re-formats the body at render time; the bubble only paints the document the manager
/// already built (render-model.md § The boundary).
///
/// This is the windows leg of the body-render unification (priority #1/#3): it mirrors
/// the canonical linux <c>views/conversations/document.rs</c> walker and the android
/// <c>DocumentText.kt</c> / web <c>document.ts</c> walkers. The producer already
/// **promotes** every remote <c>![]()</c> image out of its paragraph into a sibling
/// <c>RemoteImage</c> block in body order, so a <c>Paragraph</c>/<c>Heading</c> carries
/// text inlines only and image handling is one flat block arm. The flat-run flatten is
/// the platform-appropriate shape: the painter (<c>FaunaApp.Helpers.DocumentPainter.Apply</c>)
/// maps runs → WinUI <c>Inline</c>s, so the WinUI types stay out of <c>FaunaApp.Core</c>.
/// Replaces the former <c>MarkdownHelper.Render</c>, which re-parsed <c>msg.body</c> with
/// the shared parser at render time.
/// </summary>
internal static class DocumentRenderer
{
    private const string BulletPrefix = "• "; // "• "

    /// <summary>Walk <paramref name="doc"/> into renderable runs for the body
    /// <c>TextBlock</c> (DM bubble + feed post card/detail). The manager folds first-class
    /// embed blocks into the document — <c>Attachment</c> (conversations, D2) and
    /// <c>QuotedPost</c> / <c>Image</c> (feed, D6) / <c>LinkPreview</c> (feed, D4) after the body,
    /// and the in-bubble reply-quote <c>QuotedMessage</c> (conversations, D2) prepended before it —
    /// but each surface paints those as its own rich widget (the bubble's <c>AttachmentsList</c> +
    /// the <c>dm-message-quote</c> card; the feed's <c>quoted-post</c> Border + <c>post-image</c>
    /// Image + <c>link-preview-card</c> Border, sourced via <see cref="Attachments"/> /
    /// <see cref="QuotedMessage"/> / <see cref="QuotedPost"/> / <see cref="MediaImage"/> /
    /// <see cref="LinkPreview"/>). A body <c>RemoteImage</c> joins them (render-model.md § D3 +
    /// § Implementation status; apps/tui.md § Rendering): it minted its own ui.yaml element,
    /// <c>doc-remote-image</c>, on 2026-07-31, so — like the other five — it is painted by the
    /// PAGE as that element (<see cref="RemoteImages"/>), not as inline body text. All six are
    /// therefore excluded from the body walk — otherwise the inter-block break would leave a
    /// trailing blank line in the body for each (and the reply-quote's author/snippet would leak
    /// into <c>dm-message-text</c>).</summary>
    internal static List<MarkdownRun> Flatten(RenderDocument doc)
    {
        var runs = new List<MarkdownRun>();
        AppendBlocks(runs, BodyBlocks(doc));
        return runs;
    }

    /// <summary>The blocks the body walk paints as text — every top-level block that is not
    /// one of the embeds the page paints as its own element (see <see cref="Flatten"/>).</summary>
    private static RenderBlock[] BodyBlocks(RenderDocument doc) => doc.blocks
        .Where(b => b is not (RenderBlock.Attachment or RenderBlock.QuotedPost or RenderBlock.Image or RenderBlock.Video or RenderBlock.ProxiedImage or RenderBlock.ProxiedVideo or RenderBlock.QuotedMessage or RenderBlock.LinkPreview or RenderBlock.RemoteImage))
        .ToArray();

    /// <summary>The body as PAINT UNITS — the windows consumer of the shared text-block line-run
    /// projection (<c>fauna_core::render::inline_line_runs</c> / <c>text_line_runs</c>,
    /// render-model.md § Where logic lives + § Implementation status today). One segment per
    /// top-level body block, in body order, except that a <c>Paragraph</c> or <c>CodeBlock</c>
    /// with more hard line breaks than <c>MAX_LINES_PER_TEXT_RUN</c> yields one segment per
    /// run the shared split returns — the budget and the cut both live in Rust, this only
    /// walks each run exactly as <see cref="Flatten"/> walks the whole block.
    /// <para>Why: a single fresh layout of a multi-megabyte WinUI <c>TextBlock</c> costs tens
    /// of seconds (mail-message-size.md § Implementation status today), and the cost is the
    /// text handed to ONE widget, not the break count. <c>DocumentBodyView</c> paints a body with
    /// <see cref="BodyPaint.Split"/> false as one <c>TextBlock</c> (byte-identical to
    /// <see cref="Flatten"/>, so every ordinary message paints exactly as before) and a split
    /// body as one <c>TextBlock</c> per segment in a virtualizing <c>ItemsRepeater</c>, so only
    /// the on-screen runs are ever laid out — the same one-or-many shape as apple's
    /// <c>LineRunsView</c>.</para>
    /// A nested block (a list item's paragraph, a block-quoted paragraph) is never split — the
    /// producer's plain-text mail path emits top-level paragraphs only.</summary>
    internal static BodyPaint Segments(RenderDocument doc)
    {
        var segments = new List<IReadOnlyList<MarkdownRun>>();
        var split = false;
        foreach (var block in BodyBlocks(doc))
        {
            switch (block)
            {
                case RenderBlock.Paragraph p:
                {
                    var count = 0;
                    foreach (var run in uniffi.fauna_ffi.FaunaFfiMethods.RenderInlineLineRuns(p.inlines))
                    {
                        count++;
                        var segment = new List<MarkdownRun>();
                        AppendInlines(segment, run, headingLevel: 0, forceItalic: false);
                        segments.Add(segment);
                    }
                    split |= count > 1;
                    break;
                }
                case RenderBlock.CodeBlock c:
                {
                    var count = 0;
                    foreach (var run in uniffi.fauna_ffi.FaunaFfiMethods.RenderTextLineRuns(c.text))
                    {
                        count++;
                        var segment = new List<MarkdownRun>();
                        AppendCode(segment, run);
                        segments.Add(segment);
                    }
                    split |= count > 1;
                    break;
                }
                default:
                {
                    var segment = new List<MarkdownRun>();
                    AppendBlock(segment, block, forceItalic: false);
                    segments.Add(segment);
                    break;
                }
            }
        }
        return new BodyPaint(segments, split);
    }

    /// <summary>Every body <c>doc-remote-image</c> in <paramref name="doc"/>, in body order,
    /// whatever its state (render-model.md § D3 + § Implementation status — the 5th member of
    /// the lifted embed-projection family; apps/tui.md § Rendering). The page paints one
    /// <c>doc-remote-image</c> element per entry — blocked placeholder, revealed-but-loading, or
    /// painted — instead of the walker surfacing it inline (<see cref="Flatten"/> excludes
    /// <c>RemoteImage</c> for exactly this reason). **Single-sourced on the shared Rust
    /// <c>RenderDocument::remote_images</c>** via the UniFFI face
    /// <c>render_document_remote_images</c>, which recurses like its four siblings
    /// (<see cref="QuotedPost"/> et al.) — no per-app tree-walk to drift.</summary>
    internal static IReadOnlyList<RemoteImageRefOwned> RemoteImages(RenderDocument doc)
        => uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentRemoteImages(doc);

    /// <summary>Whether <paramref name="doc"/> carries ≥1 remote image that is still blocked
    /// (<c>revealed==false</c>) — the gate for the per-message / per-card
    /// <c>load-remote-content-button</c> after D3/D4: the manager projects the reveal set onto each
    /// <c>RenderBlock.RemoteImage.revealed</c> AND a <c>Resolved</c> link-preview og:image, so this
    /// reads the manager-authoritative flag (render-model.md § D3, § D4). **Single-sourced on the
    /// shared Rust <c>RenderDocument::has_blocked_remote_images</c>** via the UniFFI face
    /// <c>render_document_has_blocked_remote_images</c> — no per-app tree-walk, so a nested embed
    /// or a future blocked-content arm can never drift (the old local top-level-only
    /// <c>HasBlockedLinkPreviewImage</c> fold missed a link-preview og:image nested in a
    /// list/quote; the shared walk recurses at any depth).</summary>
    internal static bool HasBlockedRemoteImage(RenderDocument doc)
        => uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentHasBlockedRemoteImages(doc);

    /// <summary>The first-class <c>Attachment</c> blocks the conversations manager folded
    /// into <paramref name="doc"/> after the body (render-model.md § D2) — one per
    /// <c>MessageSnapshot.attachments</c> entry, in body order. Unlike the text inlines (which
    /// <see cref="Flatten"/> paints into the body <c>TextBlock</c>), attachments are block-level
    /// rich widgets with indexed <c>dm-attachment-image</c> / <c>dm-attachment-file</c> ids over
    /// a bounded count-all surface, so the DM bubble paints them in its <c>AttachmentsList</c>
    /// panel from this list instead of as body runs (<c>DmMessageBubble.RenderAttachments</c>).
    /// The manager appends them at the top level, never nested, so a flat filter matches exactly
    /// what it produces — the windows leg of D2 (mirrors linux <c>document.rs</c>'s in-walk
    /// <c>Attachment</c> arm; the placement source is the document, no sibling field).</summary>
    internal static List<RenderBlock.Attachment> Attachments(RenderDocument doc)
        => doc.blocks.OfType<RenderBlock.Attachment>().ToList();

    /// <summary>The folded <c>QuotedPost</c> embed block, or <c>null</c> when the post
    /// quotes nothing (render-model.md § D6). The feed manager folds one in **after** the
    /// body when <c>resolve_quoted_post</c> resolves the quote, so the feed paints the
    /// <c>quoted-post</c> card from this block instead of re-projecting the sibling
    /// <c>quoted_post_id</c> field (priority #4). **Single-sourced on the shared Rust
    /// <c>RenderDocument::quoted_post</c>** via the UniFFI face
    /// <c>render_document_quoted_post</c>, exactly like <see cref="HasBlockedRemoteImage"/> —
    /// the shared walk <b>recurses</b>, where this hand-rolled twin scanned the top level only,
    /// so a quote nested in a list/quote can no longer be silently missed (the per-app
    /// omission vector the whole face family exists to close; web/android/apple already
    /// delegate — render-model.md § Implementation status).</summary>
    internal static QuotedPostEmbedOwned? QuotedPost(RenderDocument doc)
        => uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentQuotedPost(doc);

    /// <summary>The folded in-bubble reply-quote <c>QuotedMessage</c> block, or <c>null</c>
    /// when the message is not a reply (or its parent is not loaded — render-model.md § D2).
    /// The conversations manager folds one in at read time (<c>thread_detail</c>'s
    /// <c>fold_reply_quotes</c>, beside the D3 reveal projection), <b>prepended</b> as the first
    /// block, when a message replies to a parent in the same thread; it carries the parent's
    /// <c>author_display</c> + a ≤2-line plaintext <c>snippet</c>. The DM bubble paints it as the
    /// <c>dm-message-quote</c> card from this block instead of re-projecting the sibling
    /// <c>reply_to</c> field (priority #4 — mirrors linux <c>build_reply_quote_card</c> + the
    /// android <c>documentQuotedMessage</c> + the web <c>quotedMessageBlock</c>). Top-level,
    /// never nested, so a flat first-match is exactly what the manager produces.</summary>
    internal static RenderBlock.QuotedMessage? QuotedMessage(RenderDocument doc)
        => doc.blocks.OfType<RenderBlock.QuotedMessage>().FirstOrDefault();

    /// <summary>The content hash of the folded media <c>Image</c> embed, or <c>null</c> when
    /// the post carries no resolved media (render-model.md § D6). The feed manager folds one
    /// in **after** the body when <c>resolve_media</c> resolves the blob hash, so the feed
    /// paints the <c>post-image</c> from this hash through its blob loader instead of the
    /// sibling <c>media_hash</c> field. **Single-sourced on the shared Rust
    /// <c>RenderDocument::first_image_hash</c>** via <c>render_document_first_image_hash</c>
    /// (recursing, unlike the top-level-only twin it replaces). Named <c>…Hash</c> rather than
    /// <c>MediaImage</c> because the shared face returns the hash, not a block — matching web's
    /// <c>mediaImageHash</c> and apple's <c>documentMediaImageHash</c>. The bytes load stays
    /// client glue (render-model.md § The boundary).</summary>
    internal static string? MediaImageHash(RenderDocument doc)
        => uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentFirstImageHash(doc);

    /// <summary>The content hash of the folded media <c>Video</c> embed, or <c>null</c> when
    /// the post carries no resolved video (render-model.md § Implementation status today —
    /// the D6b typed sibling of <see cref="Image"/>/<see cref="MediaImageHash"/>). The feed
    /// paints the <c>video-thumbnail</c> element from this hash as a play glyph + hash text —
    /// no poster frame exists to paint (<c>MediaItem::thumbnail</c>/<c>dimensions</c> are
    /// <c>None</c> from every writer), the same choice tui/linux/android/macos/ios made.
    /// **Single-sourced on the shared Rust <c>RenderDocument::first_video_hash</c>** via
    /// <c>render_document_first_video_hash</c> — the exact twin of <see cref="MediaImageHash"/>,
    /// which windows' image-hash wrapper already calls.</summary>
    internal static string? MediaVideoHash(RenderDocument doc)
        => uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentFirstVideoHash(doc);

    /// <summary>The nest-relative path of a bridged post's picture — its first folded
    /// <c>ProxiedImage</c> — or <c>null</c> when the post carries none, or carries a blob
    /// <c>Image</c>, which wins the one <c>post-image</c> slot (render-model.md § D6c; the
    /// precedence tui's <c>proxied_post_image</c> and apple's <c>documentPostImage</c> keep).
    /// A path, never a hash: the feed fetches it from the user's own nest with the session
    /// bearer (<c>INestHttpClient.GetContentAsync</c>), with no sealed-media open and no
    /// C2PA check, and paints it immediately — no reveal gate. Read off the shared Rust
    /// <c>RenderDocument::proxied_images</c> via <c>render_document_proxied_images</c>, which
    /// recurses like its siblings.</summary>
    internal static string? MediaProxiedPath(RenderDocument doc)
        => MediaImageHash(doc) is null
            ? uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentProxiedImages(doc).FirstOrDefault()?.path
            : null;

    /// <summary>The nest-relative path of a bridged post's video — its first folded
    /// <c>ProxiedVideo</c> — or <c>null</c> when the post carries none, or carries a blob
    /// <c>Video</c> (render-model.md § D6c → Proxied video; the exact twin of
    /// <see cref="MediaProxiedPath"/> for the <c>video-thumbnail</c> slot). The feed paints
    /// the path beside the play glyph where it paints the hash for a <c>Video</c>, and never
    /// byte-loads it. Read off the shared Rust <c>RenderDocument::proxied_videos</c> via
    /// <c>render_document_proxied_videos</c>.</summary>
    internal static string? MediaProxiedVideoPath(RenderDocument doc)
        => MediaVideoHash(doc) is null
            ? uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentProxiedVideos(doc).FirstOrDefault()?.path
            : null;

    /// <summary>Every <c>Resolved</c> folded <c>LinkPreview</c>, in body order (render-model.md
    /// § D4). The producer's bare-URL rule emits one (<c>state: Resolving</c>) after **each**
    /// standalone paragraph that is a single bare URL, <b>leaving the inline link in place</b>,
    /// so a body with two bare-URL paragraphs carries two previews (<c>fauna_core::render</c>'s
    /// <c>inject_link_previews</c>). **Single-sourced on the shared Rust
    /// <c>RenderDocument::resolved_link_previews</c>** via
    /// <c>render_document_resolved_link_previews</c>, which both recurses AND pre-filters to
    /// <c>Resolved</c> — so no call site re-derives the <c>PreviewState.Resolved</c> match, the
    /// same per-call-site re-derivation web and apple deleted.
    /// <para>This replaces a <c>FirstOrDefault</c> twin that returned only the FIRST preview —
    /// the lone-outlier gap recorded in render-model.md § Implementation status; the other six
    /// apps all paint every resolved preview, so windows now matches (priority #1/#4).</para>
    /// Excluded from the body <see cref="Flatten"/> walk so they leave no trailing break.</summary>
    internal static IReadOnlyList<ResolvedLinkPreviewOwned> ResolvedLinkPreviews(RenderDocument doc)
        => uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentResolvedLinkPreviews(doc);

    /// <summary>The urls of folded <c>LinkPreview</c> blocks still <c>Resolving</c>, in body
    /// order — the fire-once resolve trigger both surfaces drive (render-model.md § D4;
    /// <c>FeedPage.SyncPosts</c> / <c>ConversationsPage.RefreshDetailView</c>). **Single-sourced
    /// on the shared Rust <c>RenderDocument::resolving_link_preview_urls</c>** via
    /// <c>render_document_resolving_link_preview_urls</c>, so the trigger fires for EVERY
    /// unresolved preview rather than only the first — mirroring linux's
    /// <c>for url in …resolving_link_preview_urls()</c> loop and web's
    /// <c>resolvingLinkPreviewUrls</c>.</summary>
    internal static IReadOnlyList<string> ResolvingLinkPreviewUrls(RenderDocument doc)
        => uniffi.fauna_ffi.FaunaFfiMethods.RenderDocumentResolvingLinkPreviewUrls(doc);

    /// <summary>Render a sequence of sibling blocks, a hard break between each.</summary>
    private static void AppendBlocks(List<MarkdownRun> runs, RenderBlock[] blocks, bool forceItalic = false)
    {
        for (int i = 0; i < blocks.Length; i++)
        {
            if (i > 0)
                runs.Add(LineBreak());
            AppendBlock(runs, blocks[i], forceItalic);
        }
    }

    private static void AppendBlock(List<MarkdownRun> runs, RenderBlock block, bool forceItalic)
    {
        switch (block)
        {
            case RenderBlock.Paragraph p:
                AppendInlines(runs, p.inlines, headingLevel: 0, forceItalic);
                break;
            case RenderBlock.Heading h:
                AppendInlines(runs, h.inlines, headingLevel: h.level, forceItalic);
                break;
            case RenderBlock.ListBlock l:
                for (int li = 0; li < l.items.Length; li++)
                {
                    if (li > 0)
                        runs.Add(LineBreak());
                    // The shared model carries no item numbers — an ordered list renumbers
                    // from 1. Each item is a sub-document; the bullet/number prefixes its
                    // first block, which flows right after the marker.
                    runs.Add(new MarkdownRun(l.ordered ? $"{li + 1}. " : BulletPrefix));
                    AppendBlocks(runs, l.items[li].blocks, forceItalic);
                }
                break;
            // A GFM task list (render-model.md § D7a): a sibling of ListBlock whose items each
            // carry a checked state. A static ☐/☑ glyph prefixes the item's first block (read-side
            // render — the editable checkbox is the Notes editor's job). Recursing via AppendBlocks
            // means a RemoteImage inside a task item is still painted as a placeholder run (the
            // blocked-content gate itself is the shared FFI over the whole document, not the runs).
            case RenderBlock.TaskList t:
                for (int ti = 0; ti < t.items.Length; ti++)
                {
                    if (ti > 0)
                        runs.Add(LineBreak());
                    runs.Add(new MarkdownRun(t.items[ti].@checked ? "☑ " : "☐ "));
                    AppendBlocks(runs, t.items[ti].blocks, forceItalic);
                }
                break;
            case RenderBlock.CodeBlock c:
                AppendCode(runs, c.text);
                break;
            case RenderBlock.BlockQuote q:
                // One quoted block of inline content, rendered italic (linux/web add an
                // indent / `<blockquote>`; windows signals the quote with italics).
                AppendBlocks(runs, q.blocks, forceItalic: true);
                break;
            // A body remote image (render-model.md § D3). Top-level RemoteImage blocks are
            // filtered out before the body walk (Flatten) — the page paints them as their own
            // `doc-remote-image` elements (DocumentRenderer.RemoteImages), not inline text. This
            // arm defends a *nested* RemoteImage (e.g. inside a list item or block quote, which
            // — unlike Attachment/QuotedPost/QuotedMessage — a user CAN author): the shared
            // RemoteImages() face recurses to find it too, so painting nothing here matches what
            // the page actually renders at any depth.
            case RenderBlock.RemoteImage:
                break;
            // Trusted, already-fetched media addressed by content hash (render-model.md § D6
            // feed media). Top-level Image blocks are filtered out before the body walk
            // (Flatten) — the feed paints the media as the rich `post-image` Image widget
            // (DocumentRenderer.MediaImageHash), like an Attachment. This arm only defends a
            // *nested* Image (the manager never emits one); paint nothing.
            case RenderBlock.Image:
                break;
            // The typed video sibling (render-model.md § Implementation status today). Same
            // treatment as Image: top-level Video blocks are filtered out before the body walk
            // (Flatten) because the feed paints the media as its own `video-thumbnail` element;
            // this arm only defends a *nested* Video (the manager never emits one).
            case RenderBlock.Video:
                break;
            // A bridged post's nest-served picture (render-model.md § D6c) — the feed's
            // `post-image` slot, like Image, painted from MediaProxiedPath. This arm only
            // defends a *nested* one (the manager never emits one).
            case RenderBlock.ProxiedImage:
                break;
            // A bridged post's nest-served video (render-model.md § D6c → Proxied video) — the
            // feed's `video-thumbnail` slot, like Video, painted from MediaProxiedVideoPath.
            // This arm only defends a *nested* one (the manager never emits one).
            case RenderBlock.ProxiedVideo:
                break;
            // A first-class attachment (render-model.md § D2). Top-level attachments are filtered
            // out before the body walk (Flatten) — the windows bubble paints them as rich block
            // widgets in its AttachmentsList panel (DocumentRenderer.Attachments /
            // DmMessageBubble.RenderAttachments), unlike linux's single-flow walk. This arm only
            // defends a *nested* Attachment (the manager never emits one), so the switch stays
            // total and the panel split is explicit to a cold-read.
            case RenderBlock.Attachment:
                break;
            // A first-class quoted-post embed (render-model.md § D6). Top-level QuotedPost blocks
            // are filtered out before the body walk (Flatten) — the feed paints them as the
            // `quoted-post` Border (DocumentRenderer.QuotedPost). This arm only defends a *nested*
            // QuotedPost (the manager never emits one); paint nothing.
            case RenderBlock.QuotedPost:
                break;
            // A first-class in-bubble reply-quote (render-model.md § D2). Top-level QuotedMessage
            // blocks are filtered out before the body walk (Flatten) — the bubble paints them as the
            // dm-message-quote card (DocumentRenderer.QuotedMessage). This arm only defends a *nested*
            // QuotedMessage (the manager never emits one); paint nothing.
            case RenderBlock.QuotedMessage:
                break;
        }
    }

    /// <summary>Recurse the inline tree, accumulating emphasis/link state; emit one run per
    /// <c>Text</c>/<c>Code</c> leaf. A <c>bold &amp;&amp; italic</c> span the producer nests
    /// as <c>Bold(Italic(Text))</c> yields one run with both emphases set — mirroring the
    /// linux <c>inlines_to_markup</c> recursion.</summary>
    private static void AppendInlines(
        List<MarkdownRun> runs,
        Inline[] inlines,
        int headingLevel,
        bool forceItalic,
        bool bold = false,
        bool italic = false,
        string? link = null)
    {
        foreach (var inline in inlines)
        {
            switch (inline)
            {
                case Inline.Text t:
                    runs.Add(new MarkdownRun(t.text, Bold: bold, Italic: italic || forceItalic,
                        Link: link, HeadingLevel: headingLevel));
                    break;
                case Inline.Code c:
                    runs.Add(new MarkdownRun(c.text, Bold: bold, Italic: italic || forceItalic,
                        Monospace: true, Link: link, HeadingLevel: headingLevel));
                    break;
                case Inline.Bold b:
                    AppendInlines(runs, b.inlines, headingLevel, forceItalic, bold: true, italic: italic, link: link);
                    break;
                case Inline.Italic it:
                    AppendInlines(runs, it.inlines, headingLevel, forceItalic, bold: bold, italic: true, link: link);
                    break;
                case Inline.Link lk:
                    AppendInlines(runs, lk.inlines, headingLevel, forceItalic, bold: bold, italic: italic, link: lk.href);
                    break;
            }
        }
    }

    private static MarkdownRun LineBreak() => new(string.Empty, IsLineBreak: true);

    private static void AppendCode(List<MarkdownRun> runs, string code)
    {
        // Fenced code is raw multi-line text; render each source line monospace with a hard
        // break between, dropping a single trailing newline the producer may keep.
        var normalized = code.Replace("\r\n", "\n");
        if (normalized.EndsWith("\n"))
            normalized = normalized.Substring(0, normalized.Length - 1);

        var lines = normalized.Split('\n');
        for (int i = 0; i < lines.Length; i++)
        {
            if (i > 0)
                runs.Add(LineBreak());
            runs.Add(new MarkdownRun(lines[i], Monospace: true));
        }
    }
}
