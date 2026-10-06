package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.MemberStatus
import uniffi.fauna_client_mail_settings.MemberView

/**
 * Compose-level coverage for the stateless [MailListMembersContent] (the
 * `mail-list-members` page, mail-mass-mailing.md): the summary line, add /
 * import sheets, and the indexed member list with sticky unsubscribe /
 * resubscribe. Seeded state — no Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class MailListMembersContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun member(address: String, status: MemberStatus = MemberStatus.SUBSCRIBED) =
        MemberView(address = address, subscribedAtMs = 1_700_000_000_000L, status = status)

    // Stub resolver mirroring the shared member_status_label map (the real one is
    // an FFI call, kept out of this Robolectric-loaded Content).
    private val statusLabelStub: (MemberStatus) -> String = {
        if (it == MemberStatus.SUBSCRIBED) "Subscribed" else "Unsubscribed"
    }

    private fun render(
        members: List<MemberView> = emptyList(),
        hydrated: Boolean = true,
        subscribedCount: UInt = 0u,
        unsubscribedCount: UInt = 0u,
        statusLabel: (MemberStatus) -> String = statusLabelStub,
        onAddMember: (String) -> Unit = {},
        onBatchImport: (String) -> Unit = {},
        onUnsubscribe: (String) -> Unit = {},
        onResubscribe: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            MailListMembersContent(
                listName = "Weekly",
                members = members,
                hydrated = hydrated,
                subscribedCount = subscribedCount,
                unsubscribedCount = unsubscribedCount,
                statusLabel = statusLabel,
                onBack = {},
                onAddMember = onAddMember,
                onBatchImport = onBatchImport,
                onUnsubscribe = onUnsubscribe,
                onResubscribe = onResubscribe,
            )
        }
    }

    @Test
    fun rendersHeadingSummaryAndControls() {
        render(subscribedCount = 3u, unsubscribedCount = 1u)
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("mail-list-members-summary").assertExists()
        composeTestRule.onNodeWithTag("mail-list-members-add-button").assertExists()
        composeTestRule.onNodeWithTag("mail-list-members-import-button").assertExists()
    }

    @Test
    fun memberRowsRenderUnsubscribeAndResubscribe() {
        render(
            members = listOf(
                member("a@x.test", MemberStatus.SUBSCRIBED),
                member("b@x.test", MemberStatus.UNSUBSCRIBED),
            ),
        )
        assertEquals(2, composeTestRule.onAllNodesWithTag("mail-list-members-list-item").fetchSemanticsNodes().size)
        assertEquals(1, composeTestRule.onAllNodesWithTag("mail-list-members-list-item-unsubscribe-button").fetchSemanticsNodes().size)
        assertEquals(1, composeTestRule.onAllNodesWithTag("mail-list-members-list-item-resubscribe-button").fetchSemanticsNodes().size)
    }

    @Test
    fun statusLabelComesFromTheSharedResolver() {
        // The row's status text is driven by the injected member_status_label
        // resolver (shared single-source map), not a per-app hard-coded branch.
        render(members = listOf(member("a@x.test", MemberStatus.UNSUBSCRIBED)))
        composeTestRule.onNodeWithTag("mail-list-members-list-item-status")
            .assertTextEquals("Unsubscribed")
    }

    @Test
    fun addMemberFlow() {
        var added: String? = null
        render(onAddMember = { added = it })
        composeTestRule.onNodeWithTag("mail-list-members-add-button").performClick()
        composeTestRule.onNodeWithTag("mail-list-members-add-sheet-address-input").performTextInput("c@x.test")
        composeTestRule.onNodeWithTag("mail-list-members-add-sheet-submit-button").performScrollTo().performClick()
        assertEquals("c@x.test", added)
    }

    @Test
    fun unsubscribeFires() {
        var removed: String? = null
        render(members = listOf(member("a@x.test")), onUnsubscribe = { removed = it })
        composeTestRule.onNodeWithTag("mail-list-members-list-item-unsubscribe-button").performScrollTo().performClick()
        assertEquals("a@x.test", removed)
    }

    @Test
    fun unhydratedShowsLoadingText() {
        // Un-hydrated first paint must show a loading reason, not silently
        // render as if the (real, but not-yet-open) list had zero members
        // (`ui/README.md` rule 5).
        render(members = emptyList(), hydrated = false)
        composeTestRule.onNodeWithText("Loading members…").assertExists()
        assertEquals(0, composeTestRule.onAllNodesWithTag("mail-list-members-list-item").fetchSemanticsNodes().size)
    }

    @Test
    fun hydratedEmptyRealListShowsNoChromeAtAll() {
        // A real list's snapshot has landed with zero members: Add/Import are
        // already enabled and the 0/0 summary above already says it, so no
        // separate empty-state reason renders here — matches tui's reference
        // (apps/fauna-tui/src/settings/mail_list_members.rs) and fixes the
        // prior bug that borrowed the Lists page's "No lists yet" string
        // under a Members heading.
        render(members = emptyList(), hydrated = true)
        composeTestRule.onNodeWithText("Loading members…").assertDoesNotExist()
        composeTestRule.onNodeWithText("No lists yet").assertDoesNotExist()
        assertEquals(0, composeTestRule.onAllNodesWithTag("mail-list-members-list-item").fetchSemanticsNodes().size)
    }
}
