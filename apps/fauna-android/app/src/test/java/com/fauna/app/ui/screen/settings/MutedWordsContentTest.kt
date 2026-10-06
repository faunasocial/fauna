package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [MutedWordsContent] (the `muted-words`
 * Settings sub-page, moderation.md § Muted keywords). Renders with seeded state —
 * no Hilt, no VM, no FFI native calls — verifying the ui.yaml ids render and the
 * add/remove gestures fire. The at-render match + the shared normalize live behind
 * the FFI (in the VM/ApiClient), off this stateless Content.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MutedWordsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        words: List<String> = emptyList(),
        loaded: Boolean = true,
        error: String? = null,
        onAdd: (String) -> Unit = {},
        onRemove: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            MutedWordsContent(
                words = words,
                loaded = loaded,
                error = error,
                onBack = {},
                onAdd = onAdd,
                onRemove = onRemove,
            )
        }
    }

    @Test
    fun rendersInputAndAddButton() {
        render()
        composeTestRule.onNodeWithTag("muted-words").assertExists()
        composeTestRule.onNodeWithTag("muted-word-input").assertExists()
        composeTestRule.onNodeWithTag("muted-word-add-button").assertExists()
    }

    @Test
    fun emptyStateWhenNoWords() {
        render(words = emptyList(), loaded = true)
        composeTestRule.onNodeWithTag("muted-word-empty").assertExists()
        composeTestRule.onNodeWithTag("muted-word-list").assertDoesNotExist()
    }

    /**
     * The deterministic half of the loading-is-not-empty rule
     * (`docs/goal/ui/README.md` § *List pages: loading is not empty*): between
     * navigating and the first reply the page paints NEITHER rows nor the empty
     * state, and mints no `*-loading` id — the absence beside zero rows is the
     * third state. Dropping the `loaded &&` gate turns this red.
     */
    @Test
    fun noEmptyStateBeforeTheReadResolves() {
        render(words = emptyList(), loaded = false)
        composeTestRule.onNodeWithTag("muted-word-empty").assertDoesNotExist()
        composeTestRule.onNodeWithTag("muted-word-item").assertDoesNotExist()
        composeTestRule.onNodeWithTag("muted-word-input").assertExists()
    }

    @Test
    fun listRendersWordsWithRemove() {
        render(words = listOf("spam", "politics"))
        composeTestRule.onNodeWithTag("muted-word-list").assertExists()
        composeTestRule.onNodeWithTag("muted-word-empty").assertDoesNotExist()
        composeTestRule.onAllNodesWithTag("muted-word-item").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("muted-word-text")[0].assertTextEquals("spam")
        composeTestRule.onAllNodesWithTag("muted-word-remove-button").assertCountEquals(2)
    }

    @Test
    fun addGathersTypedTerm() {
        var added: String? = null
        render(onAdd = { added = it })
        composeTestRule.onNodeWithTag("muted-word-input").performTextInput("news")
        composeTestRule.onNodeWithTag("muted-word-add-button").performClick()
        assertEquals("news", added)
    }

    @Test
    fun removeFiresForRow() {
        var removed: String? = null
        render(words = listOf("spam"), onRemove = { removed = it })
        composeTestRule.onNodeWithTag("muted-word-remove-button").performClick()
        assertEquals("spam", removed)
    }

    @Test
    fun errorRendersWhenPresent() {
        render(error = "boom")
        composeTestRule.onNodeWithTag("error-message").assertExists()
    }
}
