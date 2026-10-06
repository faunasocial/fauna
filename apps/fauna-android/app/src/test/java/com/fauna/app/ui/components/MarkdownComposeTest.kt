package com.fauna.app.ui.components

import androidx.compose.ui.graphics.Color
import androidx.compose.ui.text.font.FontStyle
import androidx.compose.ui.text.font.FontWeight
import androidx.compose.ui.text.input.TextFieldValue
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.unit.em
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Unit coverage for the FFI-free compose-markdown glue in [MarkdownCompose]: the toolbar wrap
 * splice ([applyWrap] / [naiveMarkdownWrap]), the UTF-8→UTF-16 offset conversion, and the
 * inline-styling decoration builder ([buildComposeDecoration]). The shared *rules* themselves
 * (`wrap_selection`, `decoration_map`) are covered by `fauna_core::markdown` + `fauna-ffi` Rust
 * tests; this asserts the Kotlin splice/conversion/styling that consumes them.
 * conversations.md § Compose-field inline markdown styling + § Where logic lives.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MarkdownComposeTest {

    private val link = Color(0xFF3584E4)
    private val marker = Color(0xFF777777)
    private val codeBg = Color(0xFF222222)

    // ---- applyWrap / naiveMarkdownWrap (the toolbar splice) ---------------------------------

    @Test
    fun applyWrap_wrapsSelectionAndSelectsCore() {
        val value = TextFieldValue("bold world", TextRange(0, 4))
        val w = MarkdownWrapResult(replacement = "**bold**", beforeCore = "**", core = "bold")
        val out = applyWrap(value, w)
        assertEquals("**bold** world", out.text)
        // Re-selects the core ("bold") past the leading "**".
        assertEquals(TextRange(2, 6), out.selection)
    }

    @Test
    fun applyWrap_emptySelectionPlacesCaretBetweenMarkers() {
        val value = TextFieldValue("x", TextRange(1))
        val w = naiveMarkdownWrap("", "**", "**", "")
        val out = applyWrap(value, w)
        assertEquals("x****", out.text)
        assertEquals(TextRange(3, 3), out.selection)
    }

    @Test
    fun naiveMarkdownWrap_bracketsSelection() {
        val w = naiveMarkdownWrap("italic", "*", "*", "")
        assertEquals("*italic*", w.replacement)
        assertEquals("*", w.beforeCore)
        assertEquals("italic", w.core)
    }

    @Test
    fun naiveMarkdownWrap_emptyUsesPlaceholder() {
        val w = naiveMarkdownWrap("", "[", "](url)", "text")
        assertEquals("[text](url)", w.replacement)
        assertEquals("text", w.core)
    }

    // ---- utf8ByteToUtf16Index / utf16IndexToUtf8Byte (decoration_map / compose_decoration_plan's
    // byte offsets ↔ Compose's UTF-16 offsets) ------------------------------------------------

    @Test
    fun utf8ByteToUtf16Index_handlesAsciiAndMultibyte() {
        assertEquals(0, utf8ByteToUtf16Index("hello", 0))
        assertEquals(5, utf8ByteToUtf16Index("hello", 5))
        // "café" — é is 2 UTF-8 bytes but 1 UTF-16 unit, so byte 5 = char index 4.
        assertEquals(4, utf8ByteToUtf16Index("café x", 5))
        // "😀" — U+1F600 is 4 UTF-8 bytes and 2 UTF-16 units (a surrogate pair).
        assertEquals(2, utf8ByteToUtf16Index("😀x", 4))
    }

    @Test
    fun utf16IndexToUtf8Byte_isTheExactInverse() {
        assertEquals(0, utf16IndexToUtf8Byte("hello", 0))
        assertEquals(5, utf16IndexToUtf8Byte("hello", 5))
        assertEquals(5, utf16IndexToUtf8Byte("café x", 4))
        assertEquals(4, utf16IndexToUtf8Byte("😀x", 2))
        // Round-trips through both directions for every ASCII/multi-byte/surrogate offset above.
        for ((text, utf16) in listOf("café x" to 4, "😀x" to 2)) {
            assertEquals(utf16, utf8ByteToUtf16Index(text, utf16IndexToUtf8Byte(text, utf16)))
        }
    }

    // ---- buildComposeDecoration (show-markers dim-mode inline styling) ----------------------

    private fun decos(text: String, dimRanges: List<RevealSpan>, vararg d: MarkdownDecoration) =
        buildComposeDecoration(text, d.toList(), dimRanges, link, marker, codeBg)

    @Test
    fun buildComposeDecoration_emptyLeavesPlainText() {
        val out = buildComposeDecoration("hello", emptyList(), emptyList(), link, marker, codeBg)
        assertEquals("hello", out.text)
        assertTrue(out.spanStyles.isEmpty())
    }

    @Test
    fun buildComposeDecoration_stylesContentRanges() {
        // "a *b*": italic content over 'b' (1 span) + the two `*` markers.
        val out = decos(
            "a *b*", emptyList(),
            MarkdownDecoration(2, 3, "marker", 0),
            MarkdownDecoration(3, 4, "italic", 0),
            MarkdownDecoration(4, 5, "marker", 0),
        )
        assertEquals("a *b*", out.text)
        val italic = out.spanStyles.single { it.item.fontStyle == FontStyle.Italic }
        assertEquals(3, italic.start)
        assertEquals(4, italic.end)
    }

    @Test
    fun buildComposeDecoration_dimsOnlyMarkersInDimRanges() {
        // Two italic lines: "*a*\n*b*". `compose_show_markers_dim_ranges` (injected here as
        // fixed dimRanges, mirroring what the caret being on line 0 would produce) dims only
        // the line-1 markers; line-0 markers are revealed (un-dimmed).
        val text = "*a*\n*b*"
        val out = decos(
            text, listOf(RevealSpan(4, 5), RevealSpan(6, 7)),
            MarkdownDecoration(0, 1, "marker", 0),
            MarkdownDecoration(1, 2, "italic", 0),
            MarkdownDecoration(2, 3, "marker", 0),
            MarkdownDecoration(4, 5, "marker", 0),
            MarkdownDecoration(5, 6, "italic", 0),
            MarkdownDecoration(6, 7, "marker", 0),
        )
        val dimmed = out.spanStyles.filter { it.item.color == marker }
        // Only the two line-1 markers ([4,5) and [6,7)) are dimmed.
        assertEquals(setOf(4 to 5, 6 to 7), dimmed.map { it.start to it.end }.toSet())
        // Both italics are still styled regardless of the dim set.
        assertEquals(2, out.spanStyles.count { it.item.fontStyle == FontStyle.Italic })
    }

    @Test
    fun buildComposeDecoration_revealsMarkersNotInDimRanges() {
        val text = "*a*\n*b*"
        val ds = arrayOf(
            MarkdownDecoration(0, 1, "marker", 0),
            MarkdownDecoration(2, 3, "marker", 0),
            MarkdownDecoration(4, 5, "marker", 0),
            MarkdownDecoration(6, 7, "marker", 0),
        )
        // Only the line-0 markers are in the injected dim set this time (as if the caret had
        // moved to line 1) → line-1 markers revealed, line-0 markers dimmed.
        val out = decos(text, listOf(RevealSpan(0, 1), RevealSpan(2, 3)), *ds)
        val dimmed = out.spanStyles.filter { it.item.color == marker }.map { it.start to it.end }.toSet()
        assertEquals(setOf(0 to 1, 2 to 3), dimmed)
    }

    @Test
    fun buildComposeDecoration_headingIsBoldAndScaledByLevel() {
        // "# H": marker "# " ([0,2)) + heading content "H" ([2,3)) at level 1.
        val out = decos(
            "# H", emptyList(),
            MarkdownDecoration(0, 2, "marker", 0),
            MarkdownDecoration(2, 3, "heading", 1),
        )
        val heading = out.spanStyles.single { it.item.fontWeight == FontWeight.Bold }
        assertEquals(2, heading.start)
        assertEquals(3, heading.end)
        assertEquals(1.5.em, heading.item.fontSize)
    }

    @Test
    fun buildComposeDecoration_convertsMultibyteContentOffsets() {
        // "é *x*": é is 2 UTF-8 bytes, so decoration_map's byte offset for 'x' (4) maps to
        // UTF-16 index 3. Proves the conversion runs inside the builder.
        val out = decos(
            "é *x*", emptyList(),
            MarkdownDecoration(3, 4, "marker", 0),
            MarkdownDecoration(4, 5, "italic", 0),
            MarkdownDecoration(5, 6, "marker", 0),
        )
        val italic = out.spanStyles.single { it.item.fontStyle == FontStyle.Italic }
        assertEquals(3, italic.start)
        assertEquals(4, italic.end)
    }

    @Test
    fun contentStyle_codeUsesMonospaceBackground_unknownKindHasNoStyle() {
        // A code span is styled; a bare marker not in the dim set produces no span at all.
        val out = decos(
            "`c`", emptyList(),
            MarkdownDecoration(0, 1, "marker", 0),
            MarkdownDecoration(1, 2, "code", 0),
            MarkdownDecoration(2, 3, "marker", 0),
        )
        val code = out.spanStyles.single { it.item.background == codeBg }
        assertEquals(1, code.start)
        assertEquals(2, code.end)
        // Markers not in dimRanges are revealed → no marker-color span.
        assertNull(out.spanStyles.firstOrNull { it.item.color == marker })
    }

    // ---- buildHiddenComposeDecoration (hide-by-default mode) --------------------------------

    @Test
    fun buildHiddenComposeDecoration_concealsHideRangesAndDimsDimRanges() {
        // "a *b* c": the two `*` markers are hidden; content "b" keeps its italic style at its
        // shifted (post-splice) position.
        val plan = ComposeMarkerPlan(hide = listOf(RevealSpan(2, 3), RevealSpan(4, 5)), dim = emptyList())
        val result = buildHiddenComposeDecoration(
            "a *b* c",
            listOf(
                MarkdownDecoration(2, 3, "marker", 0),
                MarkdownDecoration(3, 4, "italic", 0),
                MarkdownDecoration(4, 5, "marker", 0),
            ),
            plan, link, marker, codeBg,
        )
        assertEquals("a b c", result.transformed.text)
        val italic = result.transformed.spanStyles.single { it.item.fontStyle == FontStyle.Italic }
        assertEquals(2, italic.start) // "b" shifted left by the removed leading "*"
        assertEquals(3, italic.end)
    }

    @Test
    fun buildHiddenComposeDecoration_dimsRevealedCaretEdgeMarkers() {
        // Caret sits inside "**bold**"'s run, so the shared rule would put its markers in `dim`
        // (revealed, not hidden) instead of `hide` — assert the dim-mode styling still applies.
        val plan = ComposeMarkerPlan(hide = emptyList(), dim = listOf(RevealSpan(0, 2), RevealSpan(6, 8)))
        val result = buildHiddenComposeDecoration(
            "**bold**",
            listOf(
                MarkdownDecoration(0, 2, "marker", 0),
                MarkdownDecoration(2, 6, "bold", 0),
                MarkdownDecoration(6, 8, "marker", 0),
            ),
            plan, link, marker, codeBg,
        )
        assertEquals("**bold**", result.transformed.text) // nothing concealed
        val dimmed = result.transformed.spanStyles.filter { it.item.color == marker }
        assertEquals(setOf(0 to 2, 6 to 8), dimmed.map { it.start to it.end }.toSet())
    }

    @Test
    fun buildHiddenComposeDecoration_offsetMappingRoundTripsAroundAConcealedRange() {
        // "a *b* c" (indices a0 ' '1 *2 b3 *4 ' '5 c6, length 7) hides [2,3) and [4,5) →
        // transformed "a b c" (a0 ' '1 b2 ' '3 c4, length 5).
        val plan = ComposeMarkerPlan(hide = listOf(RevealSpan(2, 3), RevealSpan(4, 5)), dim = emptyList())
        val result = buildHiddenComposeDecoration("a *b* c", emptyList(), plan, link, marker, codeBg)
        val mapping = result.offsetMapping

        // originalToTransformed: identity before the first gap, then each gap shifts by one.
        assertEquals(0, mapping.originalToTransformed(0))
        assertEquals(2, mapping.originalToTransformed(2)) // start of the first hidden '*'
        assertEquals(2, mapping.originalToTransformed(3)) // 'b', shifted left by 1
        assertEquals(3, mapping.originalToTransformed(4)) // start of the second hidden '*'
        assertEquals(3, mapping.originalToTransformed(5)) // ' ' before "c", shifted left by 2
        assertEquals(5, mapping.originalToTransformed(7)) // end of buffer

        // transformedToOriginal is the exact inverse at every kept-segment boundary.
        assertEquals(0, mapping.transformedToOriginal(0))
        assertEquals(3, mapping.transformedToOriginal(2)) // 'b' in "a b c" ← original 'b' at 3
        assertEquals(5, mapping.transformedToOriginal(3)) // ' ' before "c" ← original space at 5
        assertEquals(7, mapping.transformedToOriginal(5))
    }
}
