package com.fauna.app.ui.components

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_core.Inline
import uniffi.fauna_core.RenderBlock
import uniffi.fauna_core.RenderDocument

/**
 * Compose-level coverage for the stateless [DocumentBlocks] renderer — the document twin of the
 * deleted `MarkdownTextTest`. Drives it with the shared render model ([RenderDocument]) constructed
 * directly — no FFI native call (the records are plain Kotlin data classes). The *production* of
 * the document (markdown / plaintext / inbound-HTML → `RenderDocument`) is shared Rust, covered by
 * `fauna_core::render` Rust tests, so this only asserts the Compose walk.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class DocumentTextTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun text(s: String) = Inline.Text(s)

    private fun para(vararg inlines: Inline) = RenderBlock.Paragraph(inlines.toList())

    private fun doc(vararg blocks: RenderBlock) = RenderDocument(blocks.toList())

    /** A list item is a sub-document; a markdown item is one paragraph of plain text. */
    private fun item(s: String) = RenderDocument(listOf(para(text(s))))

    private fun render(document: RenderDocument) {
        composeTestRule.setContent { DocumentBlocks(document) }
    }

    @Test
    fun paragraph_rendersPlainText() {
        render(doc(para(text("Hello, world!"))))
        composeTestRule.onNodeWithText("Hello, world!").assertIsDisplayed()
    }

    @Test
    fun boldInline_rendersContent() {
        render(doc(para(text("This is "), Inline.Bold(listOf(text("bold"))), text(" text"))))
        composeTestRule.onNodeWithText("This is bold text").assertIsDisplayed()
    }

    @Test
    fun italicInline_rendersContent() {
        render(doc(para(text("This is "), Inline.Italic(listOf(text("italic"))), text(" text"))))
        composeTestRule.onNodeWithText("This is italic text").assertIsDisplayed()
    }

    @Test
    fun codeInline_rendersContent() {
        render(doc(para(text("Use "), Inline.Code("println"), text(" here"))))
        composeTestRule.onNodeWithText("Use println here").assertIsDisplayed()
    }

    @Test
    fun linkInline_rendersLabel() {
        render(
            doc(para(text("Visit "), Inline.Link("https://fauna.social", listOf(text("Fauna"))))),
        )
        composeTestRule.onNodeWithText("Visit Fauna").assertIsDisplayed()
    }

    @Test
    fun heading_rendersText() {
        render(doc(RenderBlock.Heading(2.toUByte(), listOf(text("Section Title")))))
        composeTestRule.onNodeWithText("Section Title").assertIsDisplayed()
    }

    @Test
    fun list_rendersBulletedItems() {
        render(doc(RenderBlock.ListBlock(ordered = false, items = listOf(item("Item one"), item("Item two")))))
        composeTestRule.onNode(hasText("Item one", substring = true)).assertIsDisplayed()
        composeTestRule.onNode(hasText("Item two", substring = true)).assertIsDisplayed()
    }

    @Test
    fun orderedList_rendersNumberedItems() {
        // The shared model carries no item numbers — the renderer numbers from 1.
        render(doc(RenderBlock.ListBlock(ordered = true, items = listOf(item("First"), item("Second")))))
        composeTestRule.onNode(hasText("1. First", substring = true)).assertIsDisplayed()
        composeTestRule.onNode(hasText("2. Second", substring = true)).assertIsDisplayed()
    }

    @Test
    fun blockquote_rendersQuotedText() {
        render(doc(RenderBlock.BlockQuote(listOf(para(text("quoted text"))))))
        composeTestRule.onNode(hasText("quoted text", substring = true)).assertIsDisplayed()
    }

    @Test
    fun boldItalicInline_rendersContent() {
        // The producer nests a bold+italic run as Bold(Italic(Text)).
        render(
            doc(
                para(
                    text("a "),
                    Inline.Bold(listOf(Inline.Italic(listOf(text("both"))))),
                    text(" b"),
                ),
            ),
        )
        composeTestRule.onNodeWithText("a both b").assertIsDisplayed()
    }

    @Test
    fun codeBlock_rendersRawCode() {
        render(doc(RenderBlock.CodeBlock(lang = null, text = "val x = 1")))
        composeTestRule.onNode(hasText("val x = 1", substring = true)).assertIsDisplayed()
    }

    @Test
    fun multipleBlocks_renderTogether() {
        render(
            doc(
                RenderBlock.Heading(1.toUByte(), listOf(text("Title"))),
                para(text("Body paragraph.")),
            ),
        )
        composeTestRule.onNode(hasText("Title", substring = true)).assertIsDisplayed()
        composeTestRule.onNode(hasText("Body paragraph.", substring = true)).assertIsDisplayed()
    }

    @Test
    fun remoteImage_unrevealed_rendersBlockedPlaceholderNotImage() {
        // The producer promotes every `![alt](url)` out of its paragraph into a sibling
        // RemoteImage block. A block with `revealed = false` renders blocked-by-default:
        // the alt text + the "Remote image blocked" caption, never an auto-fetched image
        // (html-mail.md § Security & privacy — the perimeter is render-side). The reveal
        // state is now per-block (render-model.md § D3, projected by the shared manager),
        // not a render-time flag passed into DocumentBlocks.
        render(
            doc(RenderBlock.RemoteImage(url = "https://img.test/c.png", alt = "a cat photo", revealed = false)),
        )
        composeTestRule.onNodeWithText("Remote image blocked").assertIsDisplayed()
        composeTestRule.onNodeWithText("a cat photo").assertIsDisplayed()
    }

    @Test
    fun remoteImage_revealed_paintsImageNotPlaceholder() {
        // Once the shared manager flips the block's `revealed` flag (via revealRemoteImages
        // + re-emit), the same walker paints the AsyncImage instead of the blocked
        // placeholder — no "Remote image blocked" caption. (Coil's AsyncImage does not
        // fetch under Robolectric, so we assert the placeholder is GONE rather than the
        // bitmap is present.)
        render(
            doc(RenderBlock.RemoteImage(url = "https://img.test/c.png", alt = "a cat photo", revealed = true)),
        )
        composeTestRule.onNodeWithText("Remote image blocked").assertDoesNotExist()
    }

    @Test
    fun documentAttachments_extractsAttachmentBlocksInOrder() {
        // The manager appends `Attachment` blocks after the body (render-model.md § D2); the
        // bubble iterates these instead of the sibling `attachments` field.
        val doc = doc(
            para(text("see these")),
            RenderBlock.Attachment("aa01", "pic.png", "image/png", 10u, true, false),
            RenderBlock.Attachment("bb02", "doc.pdf", "application/pdf", 20u, false, true),
        )
        val atts = documentAttachments(doc)
        assertEquals(2, atts.size)
        assertEquals("pic.png", atts[0].filename)
        assertTrue(atts[0].isImage)
        assertEquals("doc.pdf", atts[1].filename)
        assertFalse(atts[1].isImage)
        // A document with no attachment blocks yields an empty list.
        assertTrue(documentAttachments(doc(para(text("just text")))).isEmpty())
    }

    // Five extractors now delegate to shared UniFFI faces instead of walking the block tree in
    // Kotlin, so their contracts are owned by shared Rust and the former Kotlin twins are dropped
    // (a Kotlin re-assert would need host-JNA):
    //
    //   documentHasBlockedRemoteImage  → render_document_has_blocked_remote_images (§ D3/D4)
    //   documentQuotedPost             → render_document_quoted_post               (§ D6)
    //   documentMediaImageHash         → render_document_first_image_hash          (§ D6)
    //   documentResolvedLinkPreviews   → render_document_resolved_link_previews    (§ D4)
    //   documentResolvingLinkPreviewUrls → render_document_resolving_link_preview_urls (§ D4)
    //
    // Owning tests: fauna-ffi's `detects_a_blocked_remote_image_at_any_depth` and its
    // per-face siblings in `libs/fauna-ffi/src/render.rs`, plus `fauna_core::render`'s
    // `every_projection_recurses_into_quotes_and_list_items` — which asserts strictly MORE than
    // these Kotlin twins did, since all five shared walkers recurse into block quotes / list items
    // / task items where the Kotlin `filterIsInstance` scans were top-level only.
}
