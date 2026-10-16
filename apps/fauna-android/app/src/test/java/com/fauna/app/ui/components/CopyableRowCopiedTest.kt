package com.fauna.app.ui.components

import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * [CopyableRow]'s driver-facing copy witness: its `copied` attribute (the
 * node's `stateDescription`, which the e2e bridge's `/element/attr` answers)
 * starts empty and, once the row fires, carries the exact full value handed to
 * the clipboard — not the truncated display text. The Status page's
 * `status-actor-id-copy-btn` / `status-node-url-copy-btn` ride on it.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class CopyableRowCopiedTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun copiedStartsEmptyAndCarriesTheFullValueOnceFired() {
        val full = "a".repeat(64)
        composeTestRule.setContent {
            CopyableRow(
                label = "Actor ID",
                displayValue = "aaaaaaaa…",
                fullValue = full,
                testTag = "copy-row",
            )
        }
        val row = composeTestRule.onNodeWithTag("copy-row")
        row.assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, ""))

        row.performClick()

        row.assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, full))
    }
}
