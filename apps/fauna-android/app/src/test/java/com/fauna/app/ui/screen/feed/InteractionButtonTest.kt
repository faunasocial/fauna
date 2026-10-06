package com.fauna.app.ui.screen.feed

import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.FavoriteBorder
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.text.AnnotatedString
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * What an automation read of an interaction button returns (feed.md § Interaction
 * bar: "icon + interaction count … the count is hidden when 0"). The witness
 * `test_feed.py::test_a_count_shows_no_number_until_the_post_has_activity` reads
 * each button's text scoped to its card: it must name the icon and show a digit
 * only while the count is shown. Without the declared text a merged icon-only
 * button reads back EMPTY on android (the glyph carries no text) — linux's
 * `set_test_text` twin.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class InteractionButtonTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(count: Long) {
        composeTestRule.setContent {
            InteractionButton(
                testTag = "feed-like-button",
                icon = Icons.Default.FavoriteBorder,
                contentDescription = "Like",
                count = count,
            ) {}
        }
    }

    private fun readsAs(text: String) =
        SemanticsMatcher.expectValue(SemanticsProperties.Text, listOf(AnnotatedString(text)))

    @Test
    fun aButtonWithNoActivityNamesItsIconAndShowsNoNumber() {
        render(0)
        composeTestRule.onNodeWithTag("feed-like-button").assert(readsAs("Like"))
    }

    @Test
    fun aButtonWithActivityShowsItsCountOnce() {
        render(3)
        composeTestRule.onNodeWithTag("feed-like-button").assert(readsAs("Like 3"))
    }
}
