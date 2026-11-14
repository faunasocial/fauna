package com.fauna.app.testing

import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.boundsInWindow
import androidx.compose.ui.layout.onGloballyPositioned
import androidx.compose.ui.platform.LocalView
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.ui.components.DocumentBlocks
import com.fauna.app.ui.components.DocumentPaint
import com.fauna.app.ui.components.renderDocument
import androidx.compose.ui.Modifier
import org.json.JSONArray
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.Inline
import uniffi.fauna_core.RenderBlock
import uniffi.fauna_core.RenderDocument
import uniffi.fauna_core.TaskItem

/**
 * The agent's `document_painted` read — what `get_attr(<id>, "painted")` answers on android
 * for `feed-post-text` / `dm-message-text` (`helpers/painted_lines.py`, the
 * `document-painted-read` surface): a body's lines as [DocumentBlocks] painted them, box glyphs
 * kept, the laid-out indent as leading spaces (render-model.md § D7).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class TestAgentDocumentPaintedTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun para(s: String) = RenderBlock.Paragraph(listOf(Inline.Text(s)))
    private fun item(s: String, vararg more: RenderBlock) = RenderDocument(listOf(para(s)) + more)

    /** The witnesses' body: `- [x] packed the tent / - [ ] bought the stove`, then
     *  `- groceries` with `  - oats` nested under it. */
    private val checklist = RenderDocument(
        listOf(
            RenderBlock.TaskList(
                listOf(
                    TaskItem(checked = true, blocks = listOf(para("packed the tent"))),
                    TaskItem(checked = false, blocks = listOf(para("bought the stove"))),
                ),
            ),
            RenderBlock.ListBlock(
                ordered = false,
                items = listOf(
                    item("groceries", RenderBlock.ListBlock(ordered = false, items = listOf(item("oats")))),
                ),
            ),
        ),
    )

    @Before fun record() { DocumentPaint.recording = true }
    @After fun stop() { DocumentPaint.recording = false }

    @Test
    fun aNestedListIsPaintedOneLevelInFromItsParent() {
        val painted = renderDocument(checklist, Color.Blue, Color.Gray).text.split('\n')
        assertTrue(painted.contains("• groceries"))
        assertTrue("the nested item leads with an em space: $painted", painted.contains(" • oats"))
        assertTrue(painted.contains("☑ packed the tent"))
        assertTrue(painted.contains("☐ bought the stove"))
    }

    /** The rule over geometry: the lead is the first visible glyph's x, one space per 8 dp. */
    @Test
    fun paintedLinesReadTheLaidOutIndentNotTheText() {
        val text = "☑ done\n\n• parent\n • child"
        val childStart = text.indexOf(' ')
        // density 2: 8 dp = 16 px per space; the child's glyph laid out 32 px in.
        val lines = DocumentPaint.paintedLines(text, pxPerSpace = 16f) { offset ->
            if (offset == childStart + 1) 32f else 0f
        }
        assertEquals(listOf("☑ done", "• parent", "  • child"), lines)
        // A body that painted its nesting flat reads flat, whatever its text holds.
        assertEquals(
            listOf("☑ done", "• parent", "• child"),
            DocumentPaint.paintedLines(text, pxPerSpace = 16f) { 0f },
        )
    }

    @Test
    fun theReadAnswersTheBodyPaintedUnderTheFrame() {
        var bounds: Rect? = null
        val origin = IntArray(2)
        composeTestRule.setContent {
            val view = LocalView.current
            DocumentBlocks(
                checklist,
                modifier = Modifier.onGloballyPositioned {
                    bounds = it.boundsInWindow()
                    view.getLocationOnScreen(origin)
                },
            )
        }
        composeTestRule.waitForIdle()
        assertNotNull("the body was laid out", bounds)
        val b = bounds!!
        val frame = "${origin[0] + b.left.toInt()},${origin[1] + b.top.toInt()}," +
            "${b.width.toInt()},${b.height.toInt()}"

        val json = TestAgent.documentPaintedJson(frame)
        assertNotNull("a painted body under its own frame answers", json)
        val lines = JSONArray(json).let { a -> List(a.length()) { a.getString(it) } }
        assertEquals(
            listOf("packed the tent", "bought the stove", "groceries", "oats"),
            lines.map { it.trim().drop(2) },
        )
        assertTrue(lines[0].trim().startsWith("☑ ") && lines[1].trim().startsWith("☐ "))
    }

    /** No body under the frame (or no frame) is a refusal, never an empty read. */
    @Test
    fun noBodyUnderTheFrameIsNull() {
        assertNull(TestAgent.documentPaintedJson(null))
        assertNull(TestAgent.documentPaintedJson("-500,-500,1,1"))
    }
}
