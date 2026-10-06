package com.fauna.app.ui.screen.folders

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.p2pshare.GroupInvitation
import com.fauna.app.p2pshare.GroupScope
import com.fauna.app.p2pshare.OfflinePanel
import com.fauna.ffi.FfiPendingShare
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the co-present offline-share affordance on the
 * Folders page (`docs/goal/behavior/p2p.md` § Offline share initiation → *Built
 * — the affordance, both roles*). [FoldersContent] is handed an already-decided
 * [OfflineSharePaint] — every gate on it is shared Rust's answer
 * (`offline_share_gates`), resolved at the Screen — so what this pins is that
 * the page paints exactly what it is told, under the approved ids, and routes
 * each gesture to the right act: the two panels mutually exclusive, Begin and
 * Expect never both, cancel always a way out, and the consent card addressing
 * the scope id rather than a row position.
 *
 * The cross-app witness is `tests/e2e-unified/tests/test_offline_share_initiation.py`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class FoldersOfflineShareContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val ownCode = "ab".repeat(32) + "-c0a80105d431"

    private fun paint(
        panel: OfflinePanel = OfflinePanel.CLOSED,
        peerCode: String = "",
        statusText: String = "Not started",
        codeHint: String? = null,
        canBegin: Boolean = false,
        canExpect: Boolean = false,
    ) = OfflineSharePaint(
        panel = panel,
        ownCode = ownCode,
        peerCode = peerCode,
        statusText = statusText,
        codeHint = codeHint,
        showsEntryButtons = panel == OfflinePanel.CLOSED,
        showsCodeWidgets = panel != OfflinePanel.CLOSED,
        canBegin = canBegin,
        canExpect = canExpect,
        showsCancel = panel != OfflinePanel.CLOSED,
    )

    private fun invitation(scopeId: ByteArray = ByteArray(32) { 7 }) = GroupInvitation(
        scopeId = scopeId,
        initiator = "a1b2c3d4",
        shortId = "07070707",
    )

    private fun render(
        offlineShare: OfflineSharePaint? = paint(),
        actions: FoldersActions = FoldersActions(),
        pendingShares: List<FfiPendingShare> = emptyList(),
        groupInvitations: List<GroupInvitation> = emptyList(),
        groupScopes: List<GroupScope> = emptyList(),
    ) {
        composeTestRule.setContent {
            FoldersContent(
                snapshot = null,
                actions = actions,
                wizardActions = WizardActions(),
                pendingShares = pendingShares,
                offlineShare = offlineShare,
                groupInvitations = groupInvitations,
                groupScopes = groupScopes,
            )
        }
    }

    private fun node(tag: String) = composeTestRule.onNodeWithTag(tag)

    // ── The affordance ────────────────────────────────────────────────────────

    @Test
    fun noPaintMeansNoAffordanceAtAll() {
        // No usable identity (or the view could not be computed): an affordance
        // that cannot work is worse than an absent one.
        render(offlineShare = null)
        node("offline-share-button").assertDoesNotExist()
        node("offline-receive-button").assertDoesNotExist()
        node("offline-share-own-code").assertDoesNotExist()
    }

    @Test
    fun theClosedPanelOffersBothRolesAndNothingElse() {
        var opened = ""
        render(
            actions = FoldersActions(
                onOpenOfflineShare = { opened += "share;" },
                onOpenOfflineReceive = { opened += "receive;" },
            ),
        )
        node("offline-share-button").performScrollTo().performClick()
        node("offline-receive-button").performScrollTo().performClick()
        assertEquals("share;receive;", opened)
        node("offline-share-own-code").assertDoesNotExist()
        node("offline-share-peer-code-input").assertDoesNotExist()
        node("offline-share-cancel-button").assertDoesNotExist()
    }

    @Test
    fun theInitiatorPanelShowsTheCodeVerbatimAndBeginNotExpect() {
        render(offlineShare = paint(panel = OfflinePanel.INITIATE, statusText = "Invitation sent"))
        // The compare code is the ceremony's only MITM defence — it must be the
        // whole string, never truncated or reformatted.
        node("offline-share-own-code").performScrollTo().assertTextEquals(ownCode)
        node("offline-share-peer-code-input").assertExists()
        node("offline-share-begin-button").performScrollTo().assertIsNotEnabled()
        node("offline-receive-expect-button").assertDoesNotExist()
        node("offline-share-status").performScrollTo().assertTextEquals("Invitation sent")
        node("offline-share-cancel-button").assertExists()
        // The two panels are mutually exclusive with the entry buttons.
        node("offline-share-button").assertDoesNotExist()
        node("offline-receive-button").assertDoesNotExist()
    }

    @Test
    fun beginFollowsTheSharedGateAndFires() {
        var begun = false
        render(
            offlineShare = paint(panel = OfflinePanel.INITIATE, peerCode = "cd".repeat(32), canBegin = true),
            actions = FoldersActions(onBeginOfflineShare = { begun = true }),
        )
        node("offline-share-begin-button").performScrollTo().assertIsEnabled().performClick()
        assertTrue(begun)
    }

    @Test
    fun theRecipientPanelShowsExpectNotBegin() {
        var expected = false
        render(
            offlineShare = paint(panel = OfflinePanel.RECEIVE, peerCode = "cd".repeat(32), canExpect = true),
            actions = FoldersActions(onExpectOfflineShare = { expected = true }),
        )
        node("offline-share-own-code").performScrollTo().assertTextEquals(ownCode)
        node("offline-share-begin-button").assertDoesNotExist()
        node("offline-receive-expect-button").performScrollTo().assertIsEnabled().performClick()
        assertTrue(expected)
    }

    @Test
    fun typingTheirCodeReachesTheVmVerbatim() {
        var typed = ""
        render(
            offlineShare = paint(panel = OfflinePanel.INITIATE),
            actions = FoldersActions(onOfflinePeerCodeChanged = { typed = it }),
        )
        node("offline-share-peer-code-input").performScrollTo().performTextInput("cd cd")
        assertEquals("cd cd", typed)
    }

    @Test
    fun aRefusedCodeIsExplainedBesideTheInputNotOnTheErrorBar() {
        render(
            offlineShare = paint(
                panel = OfflinePanel.INITIATE,
                peerCode = ownCode,
                codeHint = "That is this device's own code — type the other person's.",
            ),
        )
        composeTestRule.onNodeWithText("That is this device's own code — type the other person's.")
            .performScrollTo().assertExists()
        node("offline-share-begin-button").performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun cancelStaysLiveWhileTheRecipientWaits() {
        // An in-flight ceremony can be abandoned — the expectation is the one
        // thing the recipient must always be able to withdraw (rule 6).
        var cancelled = 0
        render(
            offlineShare = paint(panel = OfflinePanel.RECEIVE, statusText = "Waiting for their invitation…"),
            actions = FoldersActions(onCancelOfflineShare = { cancelled++ }),
        )
        node("offline-share-cancel-button").performScrollTo().assertIsEnabled().performClick()
        assertEquals(1, cancelled)
    }

    // ── The consent card (the knock trio's group arm) ────────────────────────

    @Test
    fun anInvitationIsAKnockCardAddressedByScopeId() {
        val scope = ByteArray(32) { it.toByte() }
        var accepted: ByteArray? = null
        var declined: ByteArray? = null
        render(
            groupInvitations = listOf(invitation(scope)),
            actions = FoldersActions(
                onAcceptGroupShare = { accepted = it },
                onDeclineGroupShare = { declined = it },
            ),
        )
        node("folder-pending-share").assertExists()
        composeTestRule.onNodeWithText("a1b2c3d4 wants to share a folder with you (07070707)").assertExists()
        node("folder-share-accept-button").performScrollTo().performClick()
        node("folder-share-decline-button").performScrollTo().performClick()
        assertArrayEquals(scope, accepted)
        assertArrayEquals(scope, declined)
    }

    @Test
    fun invitationsAndKnocksAreOneListOfThingsAwaitingAnAnswer() {
        render(
            pendingShares = listOf(
                FfiPendingShare(
                    inboxId = 1L,
                    sharedBy = "ab".repeat(32),
                    sharedByHandle = null,
                    sharedByDisplay = "alice@fauna.social",
                    groupId = "cd".repeat(32),
                    channelId = "ef".repeat(32),
                    setName = "photos",
                ),
            ),
            groupInvitations = listOf(invitation()),
        )
        composeTestRule.onAllNodesWithTag("folder-pending-share").assertCountEquals(2)
    }

    // ── A landed scope ────────────────────────────────────────────────────────

    @Test
    fun aLandedScopeListsAsAnOrdinaryReadOnlySetRow() {
        render(
            groupScopes = listOf(
                GroupScope(shortId = "07070707", memberCount = 2u, sharedBy = "a1b2c3d4"),
                GroupScope(shortId = "08080808", memberCount = 2u, sharedBy = null),
            ),
        )
        composeTestRule.onAllNodesWithTag("folder-row").assertCountEquals(2)
        composeTestRule.onNodeWithText("Shared folder 07070707").assertExists()
        val badges = composeTestRule.onAllNodesWithTag("folder-shared-badge")
        badges[0].assertTextEquals("Shared by a1b2c3d4")
        badges[1].assertTextEquals("Shared · 2")
        // Severance is the authority's mint, never a self-scoped drop.
        node("folder-leave-button").assertDoesNotExist()
        node("folder-delete-button").assertDoesNotExist()
    }
}
