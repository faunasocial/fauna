package com.fauna.app.ui.components

import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.Box
import androidx.compose.material3.Text
import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Rect
import androidx.compose.ui.platform.LocalTextToolbar
import androidx.compose.ui.platform.TextToolbar
import androidx.compose.ui.platform.TextToolbarStatus
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.longClick
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performTouchInput
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.testing.InertMagnifierShadow
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * `docs/goal/ui/README.md` § Text the person reads can be copied, on android's
 * shared reason line [DisabledControlReasonText] (every page's "why is this
 * control dead" text) — the platform's own selection (Compose
 * `SelectionContainer`, rule 1), and rule 3: selecting is not acting — an
 * ordinary tap on a clickable row holding the text still does exactly what it
 * did before the text was made selectable, and a long-press that selects the
 * text inside the row never fires the row's click.
 *
 * The long-press case stubs [LocalTextToolbar]: Robolectric has no action-mode
 * window for Compose's own toolbar, and the stub's `showMenu` call is the
 * observable that a selection was made (Compose shows the toolbar only once
 * the selection gesture has selected something); [InertMagnifierShadow] keeps
 * the platform magnifier, which Robolectric cannot dismiss, out of the way.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], shadows = [InertMagnifierShadow::class])
class SelectableTextTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private var rowClicks = 0

    private var selectionMenusShown = 0

    /** Counts `showMenu` calls — Compose's signal that text is now selected. */
    private val recordingToolbar = object : TextToolbar {
        override var status: TextToolbarStatus = TextToolbarStatus.Hidden
            private set

        override fun showMenu(
            rect: Rect,
            onCopyRequested: (() -> Unit)?,
            onPasteRequested: (() -> Unit)?,
            onCutRequested: (() -> Unit)?,
            onSelectAllRequested: (() -> Unit)?,
        ) {
            selectionMenusShown++
            status = TextToolbarStatus.Shown
        }

        override fun hide() {
            status = TextToolbarStatus.Hidden
        }
    }

    private fun renderInsideAClickableRow() {
        composeTestRule.setContent {
            CompositionLocalProvider(LocalTextToolbar provides recordingToolbar) {
                Box(Modifier.testTag("row").clickable { rowClicks++ }) {
                    DisabledControlReasonText(REASON)
                }
            }
        }
    }

    @Test
    fun aTapOnTheRowStillActsOnTheRow() {
        renderInsideAClickableRow()
        composeTestRule.onNodeWithTag("row").performClick()
        composeTestRule.waitForIdle()
        assertEquals(1, rowClicks)
    }

    @Test
    fun aLongPressThatSelectsTheTextDoesNotActOnTheRow() {
        renderInsideAClickableRow()
        composeTestRule.onNodeWithText(REASON).performTouchInput { longClick() }
        composeTestRule.waitForIdle()
        assertTrue(
            "the long-press selected the reason text (the selection toolbar was asked for)",
            selectionMenusShown > 0,
        )
        assertEquals("selecting the text did not click the row", 0, rowClicks)
    }

    /**
     * The control for the case above: the same long-press on plain,
     * non-selectable text in the same row DOES click it (Compose's `clickable`
     * fires on release after a long press when it has no long-click handler),
     * so the zero above is the selection consuming the press, not a gesture
     * that never reached the row.
     */
    @Test
    fun aLongPressOnPlainTextInTheRowStillClicksIt() {
        composeTestRule.setContent {
            CompositionLocalProvider(LocalTextToolbar provides recordingToolbar) {
                Box(Modifier.testTag("row").clickable { rowClicks++ }) {
                    Text(REASON)
                }
            }
        }
        composeTestRule.onNodeWithText(REASON).performTouchInput { longClick() }
        composeTestRule.waitForIdle()
        assertEquals(0, selectionMenusShown)
        assertEquals(1, rowClicks)
    }

    private companion object {
        const val REASON = "Pick a provider first"
    }
}
