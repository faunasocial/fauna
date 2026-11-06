package com.fauna.app.ui.screen.contacts

import androidx.compose.foundation.layout.Column
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.ArchiveLinkRowPaint
import com.fauna.app.core.archiveLinkRowPaints
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_archive_import_machine.ArchiveLinkState
import uniffi.fauna_archive_import_machine.ArchiveLinkView

/**
 * Phase two's Contacts-row archive-link affordances on android
 * (`archive-import.md` § Linking and merging, ruling 8; `contacts.md`
 * § User actions) — the app-level pin of tui's
 * `contacts::tests::the_archive_link_prompt_renders_only_on_its_asked_row_and_unlink_only_when_linked`.
 * The paints come from the real shared views → composers path
 * ([archiveLinkRowPaints] over the machine crate's `archiveLinkPrompt` /
 * `archiveLinkedLabel`, resolved against the generated string resources), and
 * each row renders through the production [ContactRowContent].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ContactArchiveLinkTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val a = "aa".repeat(32)
    private val b = "bb".repeat(32)
    private val c = "cc".repeat(32)

    private fun view(contact: String, state: ArchiveLinkState, unmatched: ULong) =
        ArchiveLinkView(contact, state, "facebook", "Friend Two", 0uL, unmatched)

    /** The three rows' paints, as the screen computes them per row. */
    private fun rows(views: List<ArchiveLinkView>): List<Pair<String, List<ArchiveLinkRowPaint>>> {
        val context = ApplicationProvider.getApplicationContext<android.content.Context>()
        return listOf(a to "ada", b to "bo", c to "cy").map { (peer, name) ->
            name to archiveLinkRowPaints(context, views, peer.uppercase(), name)
        }
    }

    private fun render(
        rows: List<Pair<String, List<ArchiveLinkRowPaint>>>,
        onAnswer: (ArchiveLinkRowPaint, Boolean) -> Unit = { _, _ -> },
        onUnlink: (ArchiveLinkRowPaint) -> Unit = {},
    ) {
        composeTestRule.setContent {
            Column {
                for ((name, paints) in rows) {
                    ContactRowContent(
                        primary = name,
                        publicName = null,
                        labelsLine = null,
                        onClick = null,
                        archiveLinks = paints,
                        onArchiveLinkAnswer = onAnswer,
                        onArchiveUnlink = onUnlink,
                    )
                }
            }
        }
    }

    /** The id's node sits inside the row whose `contact-name` is [name]. */
    private fun inRowOf(id: String, name: String) =
        hasTestTag(id) and hasAnyAncestor(hasTestTag("contact-row") and hasAnyDescendant(hasText(name)))

    private fun count(id: String) =
        composeTestRule.onAllNodesWithTag(id, useUnmergedTree = true).fetchSemanticsNodes().size

    @Test
    fun theArchiveLinkPromptRendersOnlyOnItsAskedRowAndUnlinkOnlyWhenLinked() {
        render(
            rows(
                listOf(
                    view(a, ArchiveLinkState.OFFERED, 0uL),
                    view(b, ArchiveLinkState.ASKED, 0uL),
                    view(c, ArchiveLinkState.LINKED, 2uL),
                ),
            ),
        )
        for (id in listOf("contact-archive-link-prompt", "contact-archive-link-yes", "contact-archive-link-no")) {
            assertEquals(id, 1, count(id))
            composeTestRule.onNode(inRowOf(id, "bo"), useUnmergedTree = true).assertExists()
        }
        for (id in listOf("contact-archive-linked", "contact-archive-unlink")) {
            assertEquals(id, 1, count(id))
            composeTestRule.onNode(inRowOf(id, "cy"), useUnmergedTree = true).assertExists()
        }
        composeTestRule.onNodeWithTag("contact-archive-link-prompt", useUnmergedTree = true)
            .assertTextEquals("Is bo the “Friend Two” from your Facebook friends?")
        composeTestRule.onNodeWithTag("contact-archive-linked", useUnmergedTree = true)
            .assertTextEquals("Linked: Facebook · 2 could not be matched to a post")
    }

    @Test
    fun declinedAndUnlinkedRowsAskNothingAndOfferNothing() {
        render(
            rows(
                listOf(
                    view(b, ArchiveLinkState.DECLINED, 0uL),
                    view(c, ArchiveLinkState.UNLINKED, 0uL),
                ),
            ),
        )
        for (id in listOf(
            "contact-archive-link-prompt", "contact-archive-link-yes", "contact-archive-link-no",
            "contact-archive-linked", "contact-archive-unlink",
        )) {
            assertEquals(id, 0, count(id))
        }
    }

    @Test
    fun theAnswersAndUnlinkCarryTheirRowsContactAndPlatform() {
        val answers = mutableListOf<Triple<String, String, Boolean>>()
        val unlinks = mutableListOf<Pair<String, String>>()
        render(
            rows(listOf(view(b, ArchiveLinkState.ASKED, 0uL), view(c, ArchiveLinkState.LINKED, 0uL))),
            onAnswer = { p, yes -> answers += Triple(p.contact, p.platform, yes) },
            onUnlink = { p -> unlinks += p.contact to p.platform },
        )
        composeTestRule.onNodeWithTag("contact-archive-link-yes", useUnmergedTree = true).performClick()
        composeTestRule.onNodeWithTag("contact-archive-link-no", useUnmergedTree = true).performClick()
        composeTestRule.onNodeWithTag("contact-archive-unlink", useUnmergedTree = true).performClick()
        assertEquals(listOf(Triple(b, "facebook", true), Triple(b, "facebook", false)), answers)
        assertEquals(listOf(c to "facebook"), unlinks)
        assertTrue(answers.none { it.first == c })
    }
}
