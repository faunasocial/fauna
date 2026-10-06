package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.ApprovedBridgeView
import uniffi.fauna_client_mail_settings.PendingBridgeView

/**
 * Compose-level coverage for the stateless [AdminBridgesPendingContent] (the admin
 * `admin-bridges-pending` page, mail-bridge-lifecycle.md § Pending approval): the
 * indexed approval cards, the source-IP dash placeholder, approve, the
 * two-click-confirm reject, and (admin.md § Approved-bridges roster) the approved
 * roster + rotate-service-user-key inline confirm. Renders with seeded state — no
 * Hilt, no VM, no FFI.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminBridgesPendingContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun bridge(
        pubkey: String = "deadbeef",
        role: String = "mta",
        sourceIp: String? = null,
    ) = PendingBridgeView(
        pubkeyHex = pubkey,
        requestedRole = role,
        sourceIp = sourceIp,
        firstSeenAt = 1_717_000_000_000uL,
    )

    private fun approvedBridge(
        pubkey: String = "deadbeef",
        role: String = "mta",
        approvedAt: ULong? = 1_717_000_000_000uL,
    ) = ApprovedBridgeView(
        pubkeyHex = pubkey,
        role = role,
        approvedAt = approvedAt,
    )

    // FFI-free stand-in for the shared Rust resolver
    // (fauna_client_mail_settings::bridge_display_name → LocalizedText key); the
    // stateful Screen injects the real resolver, the Content stays Robolectric-safe.
    // The role→name map itself is covered by shared Rust's own unit test.
    private val displayNameStub: (String) -> String = { role ->
        when (role) {
            "mda", "mail.mda" -> "Mail & calendar bridge"
            "mta", "mail.mta" -> "Mail bridge"
            else -> "Bridge"
        }
    }

    private fun render(
        pending: List<PendingBridgeView> = emptyList(),
        approved: List<ApprovedBridgeView> = emptyList(),
        working: Boolean = false,
        displayName: (String) -> String = displayNameStub,
        onApprove: (String, String) -> Unit = { _, _ -> },
        onReject: (String) -> Unit = {},
        onRotate: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            AdminBridgesPendingContent(
                pending = pending,
                approved = approved,
                working = working,
                displayName = displayName,
                onBack = {},
                onApprove = onApprove,
                onReject = onReject,
                onRotate = onRotate,
            )
        }
    }

    @Test
    fun rendersHeadingAndEmptyState() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        assertEquals(0, composeTestRule.onAllNodesWithTag("admin-bridges-pending-card").fetchSemanticsNodes().size)
    }

    @Test
    fun cardsRenderWithFields() {
        render(pending = listOf(bridge(pubkey = "aa"), bridge(pubkey = "bb", role = "mda")))
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-pending-card").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-pending-pubkey-hex").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-pending-requested-role").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-pending-source-ip").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-pending-first-seen-at").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-pending-approve-button").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-pending-reject-button").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-pending-card-name").fetchSemanticsNodes().size)
    }

    @Test
    fun cardNameMapsPerRole() {
        // admin.md § Bridge display naming: mda serves IMAP + CalDAV → names
        // calendar; mta is SMTP-only → mail only. The Content renders whatever the
        // injected resolver returns (here the FFI-free stub); the real role→name
        // map lives in shared Rust (bridge_display_name) + its own unit test.
        render(pending = listOf(bridge(pubkey = "aa", role = "mda"), bridge(pubkey = "bb", role = "mta")))
        composeTestRule.onAllNodesWithTag("admin-bridges-pending-card-name")[0]
            .assertTextEquals("Mail & calendar bridge")
        composeTestRule.onAllNodesWithTag("admin-bridges-pending-card-name")[1]
            .assertTextEquals("Mail bridge")
    }

    @Test
    fun sourceIpRendersDashWhenAbsent() {
        render(pending = listOf(bridge(sourceIp = null)))
        composeTestRule.onNodeWithTag("admin-bridges-pending-source-ip").assertTextEquals("—")
    }

    @Test
    fun approveFiresWithPubkeyAndRole() {
        var approved: List<String>? = null
        render(pending = listOf(bridge(pubkey = "cafe", role = "mda")), onApprove = { k, r -> approved = listOf(k, r) })
        composeTestRule.onNodeWithTag("admin-bridges-pending-approve-button").performScrollTo().performClick()
        assertEquals(listOf("cafe", "mda"), approved)
    }

    @Test
    fun rejectRequiresTwoClicks() {
        var rejected: String? = null
        render(pending = listOf(bridge(pubkey = "f00d")), onReject = { rejected = it })
        val rejectBtn = composeTestRule.onNodeWithTag("admin-bridges-pending-reject-button")
        rejectBtn.performScrollTo().performClick() // arm
        assertNull(rejected)
        rejectBtn.performClick() // confirm
        assertEquals("f00d", rejected)
    }

    @Test
    fun approvedCardsRenderWithFields() {
        render(approved = listOf(approvedBridge(pubkey = "aa"), approvedBridge(pubkey = "bb", role = "mda")))
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-approved-card").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-approved-card-name").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-approved-role").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-approved-pubkey-hex").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-approved-approved-at").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-bridges-approved-rotate-button").fetchSemanticsNodes().size)
    }

    @Test
    fun rotateConfirmShowsNoDkimWarningForAnyRole() {
        for (role in listOf("mta", "mda")) {
            render(approved = listOf(approvedBridge(pubkey = "aa", role = role)))
            composeTestRule.onNodeWithTag("admin-bridges-approved-rotate-button").performScrollTo().performClick()
            composeTestRule.onNodeWithTag("admin-bridges-rotate-warning-text").assertExists()
            composeTestRule.onAllNodesWithTag("admin-bridges-rotate-dkim-warning-text").assertCountEquals(0)
        }
    }

    @Test
    fun rotateConfirmDispatchesOnConfirmAndClosesOnCancel() {
        var rotated: String? = null
        render(approved = listOf(approvedBridge(pubkey = "cafe")), onRotate = { rotated = it })
        composeTestRule.onNodeWithTag("admin-bridges-approved-rotate-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-bridges-rotate-cancel-button").performScrollTo().performClick()
        assertNull(rotated)
        composeTestRule.onNodeWithTag("admin-bridges-approved-rotate-button").assertExists()

        composeTestRule.onNodeWithTag("admin-bridges-approved-rotate-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-bridges-rotate-confirm-button").performScrollTo().performClick()
        assertEquals("cafe", rotated)
    }
}
