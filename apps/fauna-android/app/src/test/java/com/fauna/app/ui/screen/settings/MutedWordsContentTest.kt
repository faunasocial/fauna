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
        levels: Map<String, String> = emptyMap(),
        loaded: Boolean = true,
        busy: Boolean = false,
        error: String? = null,
        onAdd: (String) -> Unit = {},
        onRemove: (String) -> Unit = {},
        onSetLevel: (String, String) -> Unit = { _, _ -> },
    ) {
        composeTestRule.setContent {
            MutedWordsContent(
                rows = words.map { MutedWordRowState(it, levels[it] ?: "hide") },
                levelOptions = listOf("hide" to "Hide", "show-less" to "Show less"),
                loaded = loaded,
                busy = busy,
                error = error,
                onBack = {},
                onAdd = onAdd,
                onRemove = onRemove,
                onSetLevel = onSetLevel,
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

    /** Each row paints its level picker from the row's level token (the whole
     *  difference a soft row shows), and picking the other option reports the
     *  TOKEN, never the label. */
    @Test
    fun levelPickerPaintsTheRowsLevelAndReportsThePickedToken() {
        var picked: Pair<String, String>? = null
        render(
            words = listOf("spam", "lottery"),
            levels = mapOf("lottery" to "show-less"),
            onSetLevel = { term, token -> picked = term to token },
        )
        composeTestRule.onAllNodesWithTag("muted-word-level-select").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("muted-word-level-select")[0].assertTextEquals("Hide")
        composeTestRule.onAllNodesWithTag("muted-word-level-select")[1].assertTextEquals("Show less")
        composeTestRule.onAllNodesWithTag("muted-word-level-select")[0].performClick()
        // The menu's option, not the second row's own picker, which paints
        // "Show less" too.
        composeTestRule.onNode(hasText("Show less") and hasAnyAncestor(isPopup()))
            .performClick()
        assertEquals("spam" to "show-less", picked)
    }

    /** A round trip in flight desensitizes the picker, the remove button and
     *  the add button together. */
    @Test
    fun theRowControlsWaitWhileARoundTripIsInFlight() {
        render(words = listOf("spam"), busy = true)
        composeTestRule.onNodeWithTag("muted-word-level-select").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("muted-word-remove-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("muted-word-add-button").assertIsNotEnabled()
    }

    @Test
    fun errorRendersWhenPresent() {
        render(error = "boom")
        composeTestRule.onNodeWithTag("error-message").assertExists()
    }
}
