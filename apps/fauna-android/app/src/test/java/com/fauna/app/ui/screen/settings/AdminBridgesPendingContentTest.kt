package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import androidx.compose.runtime.getValue
import androidx.compose.runtime.remember
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_client_mail_settings.ApprovedBridgeView
import uniffi.fauna_client_mail_settings.BridgeConfirm
import uniffi.fauna_client_mail_settings.PendingBridgeView

/**
 * Compose-level coverage for the stateless [AdminBridgesPendingContent] (the admin
 * `admin-bridges-pending` page, mail-bridge-lifecycle.md § Pending approval): the
 * indexed approval cards, the source-IP dash placeholder, approve, the inline
 * reject confirm, and (admin.md § Approved-bridges roster) the approved roster +
 * rotate-service-user-key inline confirm. Both confirms are the shared machine's
 * `arm` / `disarm` / `Confirm`: the harness holds `armed` the way the VM mirrors the
 * machine's snapshot. Renders with seeded state — no Hilt, no VM, no FFI.
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
        onConfirm: (BridgeConfirm?) -> Unit = {},
        formatTimestamp: (Long) -> String = { ms -> "ts:$ms" },
    ) {
        composeTestRule.setContent {
            var armed by remember { mutableStateOf<BridgeConfirm?>(null) }
            AdminBridgesPendingContent(
                pending = pending,
                approved = approved,
                working = working,
                displayName = displayName,
                onBack = {},
                onApprove = onApprove,
                armed = armed,
                onArm = { armed = it },
                onDisarm = { armed = null },
                onConfirm = {
                    onConfirm(armed)
                    armed = null
                },
                formatTimestamp = formatTimestamp,
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
    fun timestampsRouteThroughInjectedFormatter() {
        // value-formatting.md § Absolute local timestamp display: first-seen and
        // approved-at both render through the shared formatter the Screen injects,
        // never a locale-native DateFormat. An absent approved-at keeps the dash.
        render(
            pending = listOf(bridge()),
            approved = listOf(approvedBridge(pubkey = "aa"), approvedBridge(pubkey = "bb", approvedAt = null)),
        )
        composeTestRule.onNodeWithTag("admin-bridges-pending-first-seen-at")
            .performScrollTo().assertTextEquals("ts:1717000000000")
        composeTestRule.onAllNodesWithTag("admin-bridges-approved-approved-at")[0]
            .performScrollTo().assertTextEquals("ts:1717000000000")
        composeTestRule.onAllNodesWithTag("admin-bridges-approved-approved-at")[1]
            .performScrollTo().assertTextEquals("—")
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
    fun rejectAsksFirstAndOnlyTheConfirmRejects() {
        // mail-bridge-lifecycle.md § Pending approval: Reject opens a confirm that says
        // the bridge cannot connect again until an admin re-approves a fresh keypair;
        // cancel closes it, only the confirm runs the armed act.
        var confirmed: BridgeConfirm? = null
        render(pending = listOf(bridge(pubkey = "f00d")), onConfirm = { confirmed = it })
        composeTestRule.onAllNodesWithTag("admin-bridges-reject-warning-text").assertCountEquals(0)
        composeTestRule.onNodeWithTag("admin-bridges-pending-reject-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-bridges-reject-confirm").assertExists()
        composeTestRule.onNodeWithTag("admin-bridges-reject-warning-text").assertTextContains(
            "It will not be able to connect again until an admin re-approves a fresh keypair.",
        )
        assertNull(confirmed)
        composeTestRule.onNodeWithTag("admin-bridges-reject-cancel-button").performScrollTo().performClick()
        assertNull(confirmed)
        composeTestRule.onAllNodesWithTag("admin-bridges-reject-warning-text").assertCountEquals(0)

        composeTestRule.onNodeWithTag("admin-bridges-pending-reject-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-bridges-reject-confirm-button").performScrollTo().performClick()
        assertEquals(BridgeConfirm.Reject(pubkeyHex = "f00d"), confirmed)
    }

    @Test
    fun rejectConfirmOpensOnlyUnderTheCardItNames() {
        render(pending = listOf(bridge(pubkey = "aa"), bridge(pubkey = "bb")))
        composeTestRule.onAllNodesWithTag("admin-bridges-pending-reject-button")[1].performScrollTo().performClick()
        composeTestRule.onAllNodesWithTag("admin-bridges-reject-confirm").assertCountEquals(1)
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
        // One composition per test (`setContent` refuses a second call): both
        // roles on the page, each card's rotate dialog opened and cancelled in turn.
        render(approved = listOf(approvedBridge(pubkey = "aa", role = "mta"), approvedBridge(pubkey = "bb", role = "mda")))
        for (card in 0..1) {
            composeTestRule.onAllNodesWithTag("admin-bridges-approved-rotate-button")[card].performScrollTo().performClick()
            composeTestRule.onNodeWithTag("admin-bridges-rotate-warning-text").assertExists()
            composeTestRule.onAllNodesWithTag("admin-bridges-rotate-dkim-warning-text").assertCountEquals(0)
            composeTestRule.onNodeWithTag("admin-bridges-rotate-cancel-button").performScrollTo().performClick()
        }
    }

    @Test
    fun rotateConfirmDispatchesOnConfirmAndClosesOnCancel() {
        var rotated: BridgeConfirm? = null
        render(approved = listOf(approvedBridge(pubkey = "cafe")), onConfirm = { rotated = it })
        composeTestRule.onNodeWithTag("admin-bridges-approved-rotate-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-bridges-rotate-cancel-button").performScrollTo().performClick()
        assertNull(rotated)
        composeTestRule.onNodeWithTag("admin-bridges-approved-rotate-button").assertExists()

        composeTestRule.onNodeWithTag("admin-bridges-approved-rotate-button").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-bridges-rotate-confirm-button").performScrollTo().performClick()
        assertEquals(BridgeConfirm.Rotate(pubkeyHex = "cafe"), rotated)
    }
}
