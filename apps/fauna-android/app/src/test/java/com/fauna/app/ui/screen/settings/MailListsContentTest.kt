package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.ListDraft
import uniffi.fauna_client_mail_settings.ListView

/**
 * Compose-level coverage for the stateless [MailListsContent] (the `mail-lists`
 * page, mail-mass-mailing.md): the add/edit sheet with domain picker and the
 * indexed list with per-row edit / members / delete. Seeded state — no Hilt, no
 * VM; the only native call is the shared `archiveUrlNeedsConfirm` predicate.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MailListsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun list(id: String = "aa", name: String = "Weekly") = ListView(
        listIdHex = id,
        friendlyName = name,
        localPart = "news",
        localDomain = "example.com",
        address = "news@example.com",
        description = "",
        memberCount = 12u,
        lastSendAtMs = null,
        sendsToday = 0u,
        recipientsToday = 0u,
        listHelpUrl = "",
        listArchiveUrl = "",
        recipientsPerSend = null,
    )

    private fun render(
        lists: List<ListView> = emptyList(),
        localDomains: List<String> = listOf("example.com"),
        hydrated: Boolean = true,
        onCreate: (ListDraft) -> Unit = {},
        onUpdate: (String, ListDraft) -> Unit = { _, _ -> },
        onDelete: (String) -> Unit = {},
        onViewMembers: (ListView) -> Unit = {},
    ) {
        composeTestRule.setContent {
            MailListsContent(
                lists = lists,
                localDomains = localDomains,
                hydrated = hydrated,
                working = false,
                onBack = {},
                onCreate = onCreate,
                onUpdate = onUpdate,
                onDelete = onDelete,
                onViewMembers = onViewMembers,
            )
        }
    }

    @Test
    fun rendersHeadingAndAdd() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("mail-lists-add-button").assertExists()
    }

    @Test
    fun addDisabledWithoutDomain() {
        render(localDomains = emptyList())
        composeTestRule.onNodeWithTag("mail-lists-add-button").assertIsNotEnabled()
    }

    @Test
    fun listRowsRenderWithControls() {
        render(lists = listOf(list(id = "aa"), list(id = "bb", name = "Monthly")))
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-lists-list-item").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-lists-list-item-members-button").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-lists-list-item-delete-button").fetchSemanticsNodes().size)
    }

    @Test
    fun viewMembersFires() {
        var viewed: String? = null
        render(lists = listOf(list(id = "abcd")), onViewMembers = { viewed = it.listIdHex })
        composeTestRule.onNodeWithTag("mail-lists-list-item-members-button").performScrollTo().performClick()
        assertEquals("abcd", viewed)
    }

    @Test
    fun emptyHydratedShowsResolvedEmptyPlaceholder() {
        render(lists = emptyList())
        composeTestRule.onNodeWithText("No lists yet").assertExists()
    }

    @Test
    fun unhydratedShowsLoadingNotEmptyClaim() {
        // Un-hydrated first paint must not claim "No lists yet" — the page
        // does not know that yet (`ui/README.md` rule 5).
        render(lists = emptyList(), hydrated = false)
        composeTestRule.onNodeWithText("Loading your lists…").assertExists()
        composeTestRule.onNodeWithText("No lists yet").assertDoesNotExist()
    }

    @Test
    fun createSubmitsDraft() {
        var draft: ListDraft? = null
        render(onCreate = { draft = it })
        composeTestRule.onNodeWithTag("mail-lists-add-button").performClick()
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-name-input").performTextInput("News")
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-local-part-input").performScrollTo().performTextInput("news")
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-submit-button").performScrollTo().performClick()
        assertEquals("News", draft?.friendlyName)
        assertEquals("news", draft?.localPart)
        assertEquals("example.com", draft?.localDomain)
    }

    @Test
    fun offServerArchiveLinkAsksOnceBeforeSaving() {
        // `mail-mass-mailing.md` § Don't do these: the first press relabels the
        // submit with the confirm string and saves nothing; the second saves.
        var drafts = mutableListOf<ListDraft>()
        render(onCreate = { drafts.add(it) })
        composeTestRule.onNodeWithTag("mail-lists-add-button").performClick()
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-name-input").performTextInput("News")
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-local-part-input").performScrollTo().performTextInput("news")
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-list-archive-url-input")
            .performScrollTo().performTextInput("https://archive.elsewhere.example/news")
        val submit = composeTestRule.onNodeWithTag("mail-lists-add-sheet-submit-button").performScrollTo()

        submit.performClick()
        assertEquals(0, drafts.size)
        submit.assertTextEquals(
            ApplicationProvider.getApplicationContext<android.content.Context>()
                .getString(com.fauna.app.R.string.mail_lists_archive_off_server_confirm),
        )

        submit.performClick()
        assertEquals(1, drafts.size)
        assertEquals("https://archive.elsewhere.example/news", drafts[0].listArchiveUrl)
    }

    @Test
    fun editingTheArchiveLinkAfterArmingDisarms() {
        var drafts = mutableListOf<ListDraft>()
        render(onCreate = { drafts.add(it) })
        composeTestRule.onNodeWithTag("mail-lists-add-button").performClick()
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-name-input").performTextInput("News")
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-local-part-input").performScrollTo().performTextInput("news")
        val archive = composeTestRule.onNodeWithTag("mail-lists-add-sheet-list-archive-url-input").performScrollTo()
        archive.performTextInput("https://archive.elsewhere.example/news")
        val submit = composeTestRule.onNodeWithTag("mail-lists-add-sheet-submit-button").performScrollTo()

        submit.performClick()
        archive.performTextInput("x")
        submit.performClick()
        assertEquals(0, drafts.size)
    }

    @Test
    fun savedOrOwnDomainArchiveLinkSavesOnTheFirstPress() {
        var drafts = mutableListOf<ListDraft>()
        render(
            lists = listOf(list().copy(listArchiveUrl = "https://archive.elsewhere.example/news")),
            onUpdate = { _, d -> drafts.add(d) },
        )
        composeTestRule.onNodeWithTag("mail-lists-list-item-edit-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("mail-lists-add-sheet-submit-button").performScrollTo().performClick()
        assertEquals(1, drafts.size)
    }
}
