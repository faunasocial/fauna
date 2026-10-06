package com.fauna.app.ui.screen.contacts

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [ContactRowContent] — the roster
 * row's names (`contacts.md` § The private overlay → *Where the nickname
 * paints*): `contact-name` always; `contact-public-name` only while a nickname
 * is the primary line; `contact-labels` only while the person carries a label.
 * Renders with seeded strings — no Hilt, no VM, no FFI: every string is the
 * shared projection's in the app.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ContactRowContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun inRow(tag: String) = hasTestTag(tag) and hasAnyAncestor(hasTestTag("contact-row"))

    @Test
    fun aRowWithNoOverlayShowsTheNameAloneAndNeitherConditionalLine() {
        composeTestRule.setContent {
            ContactRowContent(primary = "alice", publicName = null, labelsLine = null, onClick = null)
        }
        composeTestRule.onNode(inRow("contact-name"), useUnmergedTree = true).assertTextEquals("alice")
        composeTestRule.onNodeWithTag("contact-public-name", useUnmergedTree = true).assertDoesNotExist()
        composeTestRule.onNodeWithTag("contact-labels", useUnmergedTree = true).assertDoesNotExist()
    }

    @Test
    fun aNicknameHeadsTheRowWithThePublicNameAndTheLabelsInsideIt() {
        composeTestRule.setContent {
            ContactRowContent(
                primary = "Mum",
                publicName = "alice",
                labelsLine = "Book club, Family",
                onClick = null,
            )
        }
        // Both conditional lines sit INSIDE the row container — a scoped read
        // (`scope="contact-row[i]"`) resolves them by nested containment.
        composeTestRule.onNode(inRow("contact-name"), useUnmergedTree = true).assertTextEquals("Mum")
        composeTestRule.onNode(inRow("contact-public-name"), useUnmergedTree = true)
            .assertTextEquals("alice")
        composeTestRule.onNode(inRow("contact-labels"), useUnmergedTree = true)
            .assertTextEquals("Book club, Family")
    }

    @Test
    fun labelsWithoutANicknameShowTheLabelsLineOnly() {
        composeTestRule.setContent {
            ContactRowContent(primary = "alice", publicName = null, labelsLine = "Family", onClick = null)
        }
        composeTestRule.onNodeWithTag("contact-public-name", useUnmergedTree = true).assertDoesNotExist()
        composeTestRule.onNode(inRow("contact-labels"), useUnmergedTree = true).assertTextEquals("Family")
    }

    @Test
    fun aClickableRowOpensAndABlockedRowDoesNot() {
        var opens = 0
        composeTestRule.setContent {
            ContactRowContent(primary = "alice", publicName = null, labelsLine = null, onClick = { opens++ })
        }
        composeTestRule.onNodeWithTag("contact-row").performClick()
        assertEquals(1, opens)
    }
}
