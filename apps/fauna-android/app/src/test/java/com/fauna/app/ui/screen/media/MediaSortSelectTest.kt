package com.fauna.app.ui.screen.media

import androidx.compose.ui.semantics.Role
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.hasClickAction
import androidx.compose.ui.test.hasStateDescription
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * `media-sort-select` is a `TokenSelect` over the stable keys `name` / `size` /
 * `date`: the bridge's `select(id, token)` picks and reads back the KEY, the
 * human sees the shared `mediaSortLabel` text. Labels come from the FFI call,
 * so this runs under the host library `just android-host-test` builds.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MediaSortSelectTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val tag = "media-sort-select"

    @Test
    fun theCurrentKeyReadsBackAsAToken() {
        composeTestRule.setContent { SortSelect(current = "size", onSelect = {}) }
        composeTestRule.onNodeWithTag(tag)
            .assert(hasStateDescription("size"))
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.DropdownList))
    }

    @Test
    fun pickingByKeyRoutesThatKey() {
        var chosen: String? = null
        composeTestRule.setContent { SortSelect(current = "name", onSelect = { chosen = it }) }
        composeTestRule.onNodeWithTag(tag).performClick()
        composeTestRule.onNode(hasStateDescription("date") and hasClickAction()).performClick()
        assertEquals("date", chosen)
    }
}
