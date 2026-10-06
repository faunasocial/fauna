package com.fauna.app.ui.components

import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.text.TextRange
import androidx.compose.ui.text.input.TextFieldValue
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Toolbar-level coverage: clicking a button runs the injected shared wrap rule and splices the
 * result into the field ([applyWrap]). Uses [naiveMarkdownWrap] as the deterministic injected
 * rule (the real edge-whitespace rule lives in `fauna_core::markdown::wrap_selection` and is
 * Rust-tested; this asserts the per-widget splice wiring). conversations.md § Where logic lives.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MarkdownToolbarTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun clickButton(initial: TextFieldValue, tag: String): TextFieldValue {
        var captured = initial
        composeTestRule.setContent {
            MarkdownToolbar(
                value = captured,
                onValueChange = { captured = it },
                wrap = naiveMarkdownWrap,
            )
        }
        composeTestRule.onNodeWithTag(tag).performClick()
        return captured
    }

    @Test
    fun boldButton_wrapsSelection() {
        val out = clickButton(TextFieldValue("bold world", TextRange(0, 4)), "markdown-bold-button")
        assertEquals("**bold** world", out.text)
        assertEquals(TextRange(2, 6), out.selection)
    }

    @Test
    fun italicButton_insertsMarkersAtCaretWhenNoSelection() {
        val out = clickButton(TextFieldValue("ab", TextRange(1)), "markdown-italic-button")
        assertEquals("a**b", out.text)
        assertEquals(TextRange(2, 2), out.selection)
    }

    @Test
    fun linkButton_emptySelectionInsertsLinkTemplateWithTextSelected() {
        val out = clickButton(TextFieldValue("", TextRange(0)), "markdown-link-button")
        assertEquals("[text](url)", out.text)
        // The link label "text" is selected so the user can type over it.
        assertEquals(TextRange(1, 5), out.selection)
    }
}
