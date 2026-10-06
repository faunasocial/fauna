package com.fauna.app.ui.components

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.Spacer
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.foundation.layout.size
import androidx.compose.foundation.layout.width
import androidx.compose.foundation.text.ClickableText
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.Image
import androidx.compose.material3.Icon
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.runtime.remember
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.LocalUriHandler
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.text.*
import androidx.compose.ui.text.font.FontFamily
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.style.TextDecoration
import androidx.compose.ui.unit.dp
import androidx.compose.ui.unit.em
import coil.compose.AsyncImage
import com.fauna.app.BuildConfig
import com.fauna.app.R
import uniffi.fauna_core.Inline
import uniffi.fauna_core.QuotedPostEmbedOwned
import uniffi.fauna_core.RemoteImageRefOwned
import uniffi.fauna_core.RenderBlock
import uniffi.fauna_core.RenderDocument
import uniffi.fauna_core.ResolvedLinkPreviewOwned
import social.fauna.generated.Ids

/**
 * Renders a message body from the shared semantic [RenderDocument] (render-model.md § D1) — the
 * Compose twin of linux `views/conversations/document.rs` and web `lib/document.ts`. The
 * conversations manager builds the document **once** in shared Rust
 * (`MessageSnapshot.document`, choosing the markdown / plaintext / inbound-HTML producer); this
 * client glue only walks the typed block/inline tree into an [AnnotatedString] (priority #2 —
 * one parser definition across all apps). No body is re-parsed at render time, unlike the
 * former `parseMarkdown(msg.body)` path.
 *
 * **Remote images are blocked by default** (`docs/goal/behavior/html-mail.md` § Rendering,
 * § Security & privacy): each `RemoteImage` block renders as a placeholder — never an
 * auto-fetched image — until its own [RenderBlock.RemoteImage.revealed] flag is flipped. The
 * reveal state is owned by the shared manager (render-model.md § D3): the per-message /
 * per-post `load-remote-content-button` dispatches `revealRemoteImages` to the manager, which
 * re-emits the document with `revealed = true` projected onto the opted-in blocks. This walker
 * holds **no** reveal state of its own, so a caller can never silently re-enable tracking-pixel
 * fetches. Each remote image paints under the `doc-remote-image` test tag in EVERY state —
 * blocked placeholder or [AsyncImage] once revealed — so a blocked-vs-painted assertion reads
 * the element's content rather than its existence (ui.yaml `doc-remote-image`).
 */
@OptIn(ExperimentalTextApi::class)
@Composable
fun DocumentBlocks(
    document: RenderDocument,
    modifier: Modifier = Modifier,
    style: TextStyle = MaterialTheme.typography.bodyMedium,
) {
    val linkColor = MaterialTheme.colorScheme.primary
    val codeBackground = MaterialTheme.colorScheme.surfaceVariant
    val uriHandler = LocalUriHandler.current

    val annotated = remember(document, linkColor, codeBackground) {
        renderDocument(document, linkColor, codeBackground)
    }
    val images = remember(document) { documentRemoteImages(document) }

    Column {
        ClickableText(
            text = annotated,
            style = style,
            modifier = modifier,
            onClick = { offset ->
                // No link to open in Fauna Kids (the walker emits no URL annotation).
                if (BuildConfig.KIDS) return@ClickableText
                annotated.getUrlAnnotations(offset, offset).firstOrNull()?.let {
                    uriHandler.openUri(it.item.url)
                }
            }
        )
        images.forEach { img ->
            // Reveal state is per-block, projected by the shared manager onto
            // `RemoteImage.revealed` (render-model.md § D3) — no client-side flag. Coil's
            // AsyncImage is one composable across its own loading→loaded transition, so
            // `doc-remote-image` stays present (never a separate loading placeholder) the whole
            // time it's revealed — the blocked/revealed split is the only state boundary that
            // matters for the element's presence.
            if (img.revealed) {
                AsyncImage(
                    model = img.url,
                    contentDescription = img.alt.ifEmpty { null },
                    modifier = Modifier.fillMaxWidth().padding(top = 4.dp).testTag(Ids.DOC_REMOTE_IMAGE)
                )
            } else {
                BlockedRemoteImage(alt = img.alt)
            }
        }
    }
}

/** Remote-image blocks (`![alt](url)`) in document order — the un-fetched ones the renderer
 *  blocks by default and the `load-remote-content-button` reveals. The producer promotes every
 *  remote image to its own [RenderBlock.RemoteImage] block (render-model.md § D2).
 *
 *  Single-sourced via the shared `RenderDocument::remote_images` UniFFI face, like
 *  [documentHasBlockedRemoteImage] — the recursion into list items / block quotes / task items is
 *  Rust-owned, so a remote image nested inside one can't slip past a top-level-only Kotlin scan
 *  (the former local twin's exact failure mode). */
fun documentRemoteImages(document: RenderDocument): List<RemoteImageRefOwned> =
    com.fauna.ffi.renderDocumentRemoteImages(document)

/** Whether the body carries ≥1 **un-revealed** remote image — gates the per-message /
 *  per-post `load-remote-content-button` (which dispatches `revealRemoteImages` to the
 *  manager). The shared manager projects `revealed` onto each [RenderBlock.RemoteImage], so
 *  the button hides once every remote image in the re-emitted document is revealed.
 *  Single-sourced via the shared `RenderDocument::has_blocked_remote_images` UniFFI face
 *  (`fauna_core::render`, render-model.md § D3/D4) — the recursion into list items / block
 *  quotes and the Resolved-link-preview og:image arm are Rust-owned, so a nested embed can't
 *  slip past a per-app top-level-only walk. */
fun documentHasBlockedRemoteImage(document: RenderDocument): Boolean =
    com.fauna.ffi.renderDocumentHasBlockedRemoteImages(document)

/** The `Attachment` embed blocks, in body order — the conversations manager appends one per
 *  attachment after the text (render-model.md § D2). The bubble iterates these instead of the
 *  sibling `msg.attachments` field, so attachments come from the one document like every other
 *  block; an image attachment is still painted by the caller (it needs the async
 *  `loadAttachmentBytes` loader, kept in the screen). Attachments are flat top-level blocks,
 *  so a shallow filter suffices (unlike remote images, which may nest in a quote/list). */
fun documentAttachments(document: RenderDocument): List<RenderBlock.Attachment> =
    document.blocks.filterIsInstance<RenderBlock.Attachment>()

/** The folded feed quoted-post embed, or `null` (render-model.md § D6). The feed manager folds one
 *  `QuotedPost` after the body via `resolve_quoted_post`; the feed screens paint the card from it
 *  (author + body) instead of the sibling `quotedPostId` field. Also the **fire-once guard** for
 *  `resolveQuotedPost`: fire only while this is null, so the manager's idempotent re-emit settles
 *  instead of driving a recomposition loop.
 *
 *  Single-sourced via the shared `RenderDocument::quoted_post` UniFFI face, like
 *  [documentHasBlockedRemoteImage] — the recursion into list items / block quotes is Rust-owned.
 *  The returned record also carries `authoringOrigin`, which android does **not** paint yet:
 *  `delegated-origin-badge` is tui-lead (ui.yaml, 2026-07-31) with the other six apps on the
 *  batched trickle-down, which must not precede the tui marker. */
fun documentQuotedPost(document: RenderDocument): QuotedPostEmbedOwned? =
    com.fauna.ffi.renderDocumentQuotedPost(document)

/** The in-bubble reply-quote block, or `null` (render-model.md § D2 `QuotedMessage`). The
 *  conversations manager folds one in at read time (`thread_detail`, PREPENDED above the body)
 *  when a message replies to a parent loaded in the same thread; the bubble paints the card
 *  (author + ≤ 2-line snippet) from this block instead of the sibling `replyTo` field. Hidden
 *  (null) when the parent isn't loaded. A flat top-level block, like [documentQuotedPost]. */
fun documentQuotedMessage(document: RenderDocument): RenderBlock.QuotedMessage? =
    document.blocks.filterIsInstance<RenderBlock.QuotedMessage>().firstOrNull()

/** The content hash of the first folded feed media `Image`, or `null` (render-model.md § D6). The
 *  feed manager folds an `Image { hash, alt }` after the body via `resolve_media` (the post's first
 *  attachment); the feed screens paint the hash through the client blob loader (the
 *  async-byte-load-stays-client idiom — the shared walker has no loader). `Image` is trusted/public
 *  media (not a blocked `RemoteImage`).
 *
 *  Single-sourced via the shared `RenderDocument::first_image_hash` UniFFI face — hash only,
 *  matching the shared projection. The block's `alt` is not part of it and nothing is lost: the
 *  feed fold sets `alt` to the empty string by construction (`fauna-feed` `manager.rs`: "the alt
 *  isn't on the snapshot"), and no app reads it. Should it ever become paintable it belongs on the
 *  shared face, so all 7 apps get it at once. */
fun documentMediaImageHash(document: RenderDocument): String? =
    com.fauna.ffi.renderDocumentFirstImageHash(document)

/** The content hash of the first folded feed media `Video`, or `null`
 *  (render-model.md § D6b) — the exact twin of [documentMediaImageHash], one
 *  fold apart: `media_blocks` makes the image-vs-video branch once, so a
 *  document never carries both for the same attachment. No poster frame
 *  exists to paint (`MediaItem.thumbnail`/`dimensions` are `None` from every
 *  writer, deliberately — a poster field would be dead on arrival), so the
 *  `video-thumbnail` painter reads only this hash, never blob bytes.
 *
 *  Single-sourced via the shared `RenderDocument::first_video_hash` UniFFI
 *  face. */
fun documentMediaVideoHash(document: RenderDocument): String? =
    com.fauna.ffi.renderDocumentFirstVideoHash(document)

/** The nest-relative path `post-image` paints when the post is a bridged picture — the
 *  first `ProxiedImage` of a document with no blob image (render-model.md § D6c; the
 *  precedence of the shared `RenderDocument::proxied_post_image`), or `null`. Read over
 *  the shared `render_document_proxied_images` UniFFI face. */
fun documentProxiedPostImagePath(document: RenderDocument): String? =
    if (documentMediaImageHash(document) != null) null
    else com.fauna.ffi.renderDocumentProxiedImages(document).firstOrNull()?.path

/** The nest-relative path `video-thumbnail` paints when the post is a bridged video —
 *  [documentProxiedPostImagePath]'s precedence applied to the video slot
 *  (render-model.md § D6c → *Proxied video*). */
fun documentProxiedPostVideoPath(document: RenderDocument): String? =
    if (documentMediaVideoHash(document) != null) null
    else com.fauna.ffi.renderDocumentProxiedVideos(document).firstOrNull()?.path

/** The Resolved link previews in `document`, in body order (render-model.md § D4) — the feed
 *  screen paints one card per entry. The og:image (`imageHash`) is blocked-by-default like a
 *  `RemoteImage`: painted only when `revealed` (the D3 twin). Title/description/domain always show.
 *  The screen has the blob loader (`vm.fetchBlobBytes`) the shared walker lacks.
 *
 *  Single-sourced via the shared `RenderDocument::resolved_link_previews` UniFFI face, which
 *  returns the flat card record — so the `PreviewState.Resolved` match is Rust's, not re-derived
 *  per app (android's local `ResolvedLinkPreview` mirror is deleted with this swap). */
fun documentResolvedLinkPreviews(document: RenderDocument): List<ResolvedLinkPreviewOwned> =
    com.fauna.ffi.renderDocumentResolvedLinkPreviews(document)

/** The urls of `Resolving` link previews in `document` — the feed screen fires
 *  `vm.resolveLinkPreview` for each (render-model.md § D4). Single-sourced via the shared
 *  `RenderDocument::resolving_link_preview_urls` UniFFI face; fire-once by construction, since a
 *  resolved block no longer yields its url. */
fun documentResolvingLinkPreviewUrls(document: RenderDocument): List<String> =
    com.fauna.ffi.renderDocumentResolvingLinkPreviewUrls(document)

/** Blocked-by-default placeholder for a remote image: a picture glyph + the alt text + a dim
 *  "Remote image blocked" caption, with **no** image source (no fetch). Mirrors the linux /
 *  windows / apple placeholder shape (`html-mail.md` § Implementation status). `internal` so the
 *  [DocumentBlocks] walker reuses one placeholder. Carries the `doc-remote-image` test tag —
 *  the SAME id [DocumentBlocks] puts on the revealed [AsyncImage], so the element registers in
 *  every state under one id (ui.yaml `doc-remote-image`). */
@Composable
internal fun BlockedRemoteImage(alt: String) {
    Row(
        verticalAlignment = Alignment.CenterVertically,
        modifier = Modifier.padding(top = 4.dp).testTag(Ids.DOC_REMOTE_IMAGE)
    ) {
        Icon(
            Icons.Default.Image,
            contentDescription = null,
            tint = MaterialTheme.colorScheme.onSurfaceVariant,
            modifier = Modifier.size(20.dp)
        )
        Spacer(Modifier.width(8.dp))
        Column {
            if (alt.isNotBlank()) {
                Text(alt, style = MaterialTheme.typography.bodySmall)
            }
            Text(
                stringResource(R.string.conversations_detail_remote_image_blocked),
                style = MaterialTheme.typography.labelSmall,
                color = MaterialTheme.colorScheme.onSurfaceVariant
            )
        }
    }
}

@OptIn(ExperimentalTextApi::class)
private fun renderDocument(
    document: RenderDocument,
    linkColor: Color,
    codeBackground: Color
): AnnotatedString = buildAnnotatedString {
    var emitted = false
    document.blocks.forEach { block ->
        when (block) {
            // Image and Attachment blocks render as composables below the text (never inline,
            // never auto-fetched — html-mail.md § Security & privacy; attachments need the
            // async byte loader), so they contribute no AnnotatedString text and no
            // inter-block gap.
            // LinkPreview is painted as a card below the text by the feed screen (it needs the
            // blob loader for the og:image), like Image/Attachment — render-model.md § D4.
            // ProxiedImage (a bridged post's nest-served picture, render-model.md § D6c) belongs
            // to the feed screen's `post-image` slot too, and ProxiedVideo (its video twin,
            // § D6c → Proxied video) to the `video-thumbnail` slot (`FeedPostImage` /
            // `FeedPostVideo` paint both).
            is RenderBlock.RemoteImage, is RenderBlock.Image, is RenderBlock.Video,
            is RenderBlock.ProxiedImage, is RenderBlock.ProxiedVideo, is RenderBlock.Attachment,
            is RenderBlock.QuotedPost, is RenderBlock.QuotedMessage, is RenderBlock.LinkPreview -> Unit
            else -> {
                if (emitted) append("\n\n")
                appendBlock(block, linkColor, codeBackground)
                emitted = true
            }
        }
    }
}

@OptIn(ExperimentalTextApi::class)
private fun AnnotatedString.Builder.appendBlock(
    block: RenderBlock,
    linkColor: Color,
    codeBackground: Color
) {
    when (block) {
        is RenderBlock.Heading -> {
            // Relative sizing so headings scale with the surrounding text style.
            val size = when (block.level.toInt()) {
                1 -> 1.5
                2 -> 1.3
                3 -> 1.15
                else -> 1.05
            }
            withStyle(SpanStyle(fontWeight = FontWeight.Bold, fontSize = size.em)) {
                appendInlines(block.inlines, linkColor, codeBackground)
            }
        }
        is RenderBlock.Paragraph -> appendInlines(block.inlines, linkColor, codeBackground)
        is RenderBlock.ListBlock -> {
            // The shared model carries no item numbers — an ordered list renumbers from 1.
            block.items.forEachIndexed { i, item ->
                if (i > 0) append("\n")
                append(if (block.ordered) "${i + 1}. " else "• ")
                // A markdown list item is one paragraph; append its block content inline.
                item.blocks.forEachIndexed { j, b ->
                    if (j > 0) append("\n")
                    appendBlock(b, linkColor, codeBackground)
                }
            }
        }
        // A GFM task list (render-model.md § D7a): a static ☐/☑ glyph per item + its content
        // (read-side render — the editable checkbox is the Notes editor's job). Sibling of
        // ListBlock; each item is a sub-document like a list item.
        is RenderBlock.TaskList -> {
            block.items.forEachIndexed { i, item ->
                if (i > 0) append("\n")
                append(if (item.checked) "☑ " else "☐ ")
                item.blocks.forEachIndexed { j, b ->
                    if (j > 0) append("\n")
                    appendBlock(b, linkColor, codeBackground)
                }
            }
        }
        is RenderBlock.BlockQuote -> withStyle(SpanStyle(fontStyle = FontStyle.Italic)) {
            // linux/web add an indent / `<blockquote>`; Compose signals the quote with italics.
            block.blocks.forEachIndexed { i, b ->
                if (i > 0) append("\n")
                appendBlock(b, linkColor, codeBackground)
            }
        }
        is RenderBlock.CodeBlock -> withStyle(
            SpanStyle(fontFamily = FontFamily.Monospace, background = codeBackground)
        ) {
            append(block.text)
        }
        // Images, attachments, and the feed `QuotedPost` embed (render-model.md § D6) are
        // pulled out and rendered below as composables (see renderDocument, the bubble's
        // `documentAttachments` loop, and the feed screens' `documentMediaImage` /
        // `documentQuotedPost` extractors) — never as `AnnotatedString` text — so the body
        // walk doesn't double-render them.
        is RenderBlock.Image, is RenderBlock.Video, is RenderBlock.ProxiedImage,
        is RenderBlock.ProxiedVideo, is RenderBlock.RemoteImage, is RenderBlock.Attachment,
        is RenderBlock.QuotedPost, is RenderBlock.QuotedMessage,
        is RenderBlock.LinkPreview -> Unit
    }
}

@OptIn(ExperimentalTextApi::class)
private fun AnnotatedString.Builder.appendInlines(
    inlines: List<Inline>,
    linkColor: Color,
    codeBackground: Color
) {
    inlines.forEach { inline ->
        when (inline) {
            is Inline.Text -> append(inline.text)
            is Inline.Code -> withStyle(
                SpanStyle(fontFamily = FontFamily.Monospace, background = codeBackground)
            ) {
                append(inline.text)
            }
            // Fauna Kids renders a link as inert text (family-safety.md § The
            // account age band, the kids-app bullet, item (4)): its words, with no
            // URL annotation to open and no link styling to invite a tap.
            is Inline.Link -> if (BuildConfig.KIDS) {
                appendInlines(inline.inlines, linkColor, codeBackground)
            } else {
                withAnnotation(UrlAnnotation(inline.href)) {
                    withStyle(
                        SpanStyle(color = linkColor, textDecoration = TextDecoration.Underline)
                    ) {
                        appendInlines(inline.inlines, linkColor, codeBackground)
                    }
                }
            }
            // Nested emphasis maps to merged SpanStyles: the producer nests a bold+italic
            // run as Bold(Italic(Text)) → fontWeight Bold + fontStyle Italic.
            is Inline.Bold -> withStyle(SpanStyle(fontWeight = FontWeight.Bold)) {
                appendInlines(inline.inlines, linkColor, codeBackground)
            }
            is Inline.Italic -> withStyle(SpanStyle(fontStyle = FontStyle.Italic)) {
                appendInlines(inline.inlines, linkColor, codeBackground)
            }
        }
    }
}
