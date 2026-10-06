package com.fauna.app.ui.screen.profile

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [ProfilePrivateSection] and
 * [ProfileHeaderNames] (`profile.md` § The private section): every approved
 * element paints, labels render as chips with their own remove button, the add
 * field empties only when the label was staged, and the header's secondary
 * line exists only while a nickname is the primary one. Seeded state — no
 * Hilt, no VM, no FFI: staging and refusals are shared Rust's in the app.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfilePrivateSectionContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        labels: List<String> = emptyList(),
        saving: Boolean = false,
        onAddLabel: (String) -> Boolean = { true },
        onRemoveLabel: (Int) -> Unit = {},
        onSave: () -> Unit = {},
        onNickname: (String) -> Unit = {},
        onNotes: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            var nickname by remember { mutableStateOf("") }
            var notes by remember { mutableStateOf("") }
            ProfilePrivateSection(
                nickname = nickname,
                notes = notes,
                labels = labels,
                saving = saving,
                onNicknameChange = { nickname = it; onNickname(it) },
                onNotesChange = { notes = it; onNotes(it) },
                onAddLabel = onAddLabel,
                onRemoveLabel = onRemoveLabel,
                onSave = onSave,
            )
        }
    }

    @Test
    fun theSectionPaintsEveryApprovedElement() {
        render()
        for (tag in listOf(
            "profile-private-section",
            "profile-nickname-field",
            "profile-notes-field",
            "profile-label-list",
            "profile-label-field",
            "profile-label-add-button",
            "profile-private-save-button",
        )) {
            composeTestRule.onNodeWithTag(tag).assertExists()
        }
        composeTestRule.onNodeWithTag("profile-label-chip").assertDoesNotExist()
    }

    @Test
    fun labelsRenderAsChipsEachWithItsOwnRemoveButton() {
        val removed = mutableListOf<Int>()
        render(labels = listOf("Book club", "Family"), onRemoveLabel = { removed += it })
        val chips = composeTestRule.onAllNodesWithTag("profile-label-chip")
        chips.assertCountEquals(2)
        chips[0].assertTextEquals("Book club")
        chips[1].assertTextEquals("Family")
        composeTestRule.onAllNodesWithTag("profile-label-remove-button")[1]
            .performClick()
        assertEquals(listOf(1), removed)
    }

    @Test
    fun theAddFieldEmptiesOnlyWhenTheLabelWasStaged() {
        val asked = mutableListOf<String>()
        var accept = false
        render(onAddLabel = { asked += it; accept })

        composeTestRule.onNodeWithTag("profile-label-field").performTextInput("Family")
        composeTestRule.onNodeWithTag("profile-label-add-button").performClick()
        // Refused: the typed label stays, so the user can fix it.
        composeTestRule.onNodeWithTag("profile-label-field").assertTextContains("Family")

        accept = true
        composeTestRule.onNodeWithTag("profile-label-add-button").performClick()
        assertEquals(listOf("Family", "Family"), asked)
        composeTestRule.onNodeWithTag("profile-label-field").assert(hasText(""))
    }

    @Test
    fun editsAndSaveReachTheirCallbacksAndSaveIsDisabledWhileSaving() {
        val nicknames = mutableListOf<String>()
        val notes = mutableListOf<String>()
        var saves = 0
        render(onNickname = { nicknames += it }, onNotes = { notes += it }, onSave = { saves++ })
        composeTestRule.onNodeWithTag("profile-nickname-field").performTextInput("Mum")
        composeTestRule.onNodeWithTag("profile-notes-field").performTextInput("allergic to cats")
        composeTestRule.onNodeWithTag("profile-private-save-button").performClick()
        assertEquals("Mum", nicknames.last())
        assertEquals("allergic to cats", notes.last())
        assertEquals(1, saves)
    }

    @Test
    fun saveIsDisabledWhileASaveIsInFlight() {
        render(saving = true)
        composeTestRule.onNodeWithTag("profile-private-save-button").assertIsNotEnabled()
    }

    @Test
    fun theHeaderShowsThePublicNameOnlyWhileANicknameLeads() {
        var publicName by mutableStateOf<String?>("alice")
        composeTestRule.setContent {
            ProfileHeaderNames(primary = if (publicName != null) "Mum" else "alice", publicName = publicName)
        }
        composeTestRule.onNodeWithTag("profile-handle").assertTextEquals("Mum")
        composeTestRule.onNodeWithTag("profile-public-name").assertTextEquals("alice")

        // Clearing the nickname restores the one-line header.
        publicName = null
        composeTestRule.onNodeWithTag("profile-handle").assertTextEquals("alice")
        composeTestRule.onNodeWithTag("profile-public-name").assertDoesNotExist()
    }
}
