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
import uniffi.fauna_media_machine.FollowedScopeOption
import uniffi.fauna_media_machine.MediaFolderOption

/**
 * `media-folder-filter` is a `TokenSelect`: the bridge's `select(id, token)`
 * reads the token off `stateDescription`, the human reads the label. The
 * device bridge cannot run in CI, so these host pins are what can be asserted.
 * `media-sort-select` is pinned in `MediaSortSelectTest` (its labels come from
 * the `mediaSortLabel` FFI call, which runs under the host library).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MediaFilterSelectTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val own = MediaFolderOption(name = "Trip", folderId = "f1", value = "own:trip", label = "Trip")
    private val shared = MediaFolderOption(name = "Trip", folderId = "f2", value = "shared:trip", label = "Trip (Bob)")
    private val followed = FollowedScopeOption(value = "follow:1", label = "Public pics", available = true)

    private val tag = "media-folder-filter"

    private fun render(
        currentFilterLabel: String?,
        onSelect: (String?) -> Unit = {},
        onSelectFollowed: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            FilterSelect(
                currentFilterLabel = currentFilterLabel,
                options = listOf(own, shared),
                followed = listOf(followed),
                onSelect = onSelect,
                onSelectFollowed = onSelectFollowed,
            )
        }
    }

    @Test
    fun defaultCarriesTheAllSentinelAsADropdown() {
        render(currentFilterLabel = null)
        composeTestRule.onNodeWithTag(tag)
            .assert(hasStateDescription("__all__"))
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.Role, Role.DropdownList))
    }

    @Test
    fun anActiveSharedSetReadsBackItsMintedTokenNotItsLabel() {
        render(currentFilterLabel = "Trip (Bob)")
        composeTestRule.onNodeWithTag(tag).assert(hasStateDescription("shared:trip"))
    }

    @Test
    fun pickingByTokenRoutesOwnSetAndFollowedScope() {
        var chosen: String? = "unset"
        var followedChosen: String? = null
        render(
            currentFilterLabel = null,
            onSelect = { chosen = it },
            onSelectFollowed = { followedChosen = it },
        )
        fun pick(token: String) {
            composeTestRule.onNodeWithTag(tag).performClick()
            composeTestRule.onNode(hasStateDescription(token) and hasClickAction()).performClick()
        }
        pick("own:trip")
        assertEquals("own:trip", chosen)
        pick("follow:1")
        assertEquals("follow:1", followedChosen)
    }
}
