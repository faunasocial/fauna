package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiAdminHostingRow
import com.fauna.ffi.FfiReceiptState
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [AdminCustodyHostingContent] (the
 * admin `admin-custody-hosting` page): the pre-hydrate vs
 * answered-empty honesty split, indexed rows, the capless-row *Default* text,
 * and the single-armed-row remove confirm. Renders with seeded state — no
 * Hilt, no VM, no FFI network call (the `FfiAdminHostingRow`/`FfiReceiptState`
 * records themselves are FFI types, but constructing one in-process is pure —
 * mirrors `AdminBridgesPendingContentTest`).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminCustodyHostingContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun row(
        host: String = "aa".repeat(32),
        owner: String = "bb".repeat(32),
        url: String = "https://owner.example",
        grant: ByteArray = byteArrayOf(1, 2, 3),
        retainedBytesCap: Long = 8192,
        heldBytes: Long = 4096,
        stopped: Boolean = false,
        receiptState: FfiReceiptState = FfiReceiptState.FRESH,
    ) = FfiAdminHostingRow(
        hostActorId = host,
        ownerActorId = owner,
        ownerNestUrl = url,
        grantId = grant,
        retainedBytesCap = retainedBytesCap,
        heldBytes = heldBytes,
        stopped = stopped,
        receiptState = receiptState,
    )

    // FFI-free stand-ins for the shared Rust formatters (ValueFormat.byteSize /
    // com.fauna.ffi.shortId) the stateful Screen injects — a plain Robolectric
    // JVM host has no native library loaded, so the Content must never call
    // those directly (mirrors AdminBridgesPendingContentTest's displayNameStub).
    private val budgetTextStub: (Long) -> String = { cap -> if (cap == 0L) "Default" else "${cap}B" }
    private val heldTextStub: (Long) -> String = { held -> "${held}B" }
    private val shortIdStub: (String) -> String = { hex -> hex.take(8) }

    private fun render(
        rows: List<FfiAdminHostingRow>? = null,
        status: String? = null,
        working: Boolean = false,
        budgetText: (Long) -> String = budgetTextStub,
        heldText: (Long) -> String = heldTextStub,
        shortId: (String) -> String = shortIdStub,
        onRemove: (String, ByteArray) -> Unit = { _, _ -> },
    ) {
        composeTestRule.setContent {
            AdminCustodyHostingContent(
                rows = rows,
                status = status,
                working = working,
                budgetText = budgetText,
                heldText = heldText,
                shortId = shortId,
                onBack = {},
                onRemove = onRemove,
            )
        }
    }

    @Test
    fun preHydratePaintsNeitherCountNorEmptyState() {
        // The pre-hydrate/answered-empty honesty rule tui/linux/web all pin
        // with unit tests: an unanswered read must never look like a known-
        // empty one.
        render(rows = null)
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onAllNodesWithTag("admin-custody-hosting-count").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("admin-custody-hosting-empty").assertCountEquals(0)
    }

    @Test
    fun answeredEmptyListPaintsCountAndEmptyState() {
        render(rows = emptyList())
        composeTestRule.onNodeWithTag("admin-custody-hosting-count").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-empty").assertExists()
    }

    @Test
    fun rowsRenderWithFields() {
        render(rows = listOf(row(host = "aa".repeat(32)), row(host = "cc".repeat(32))))
        composeTestRule.onAllNodesWithTag("admin-custody-hosting-count").assertCountEquals(1)
        assertEquals(0, composeTestRule.onAllNodesWithTag("admin-custody-hosting-empty").fetchSemanticsNodes().size)
        assertEquals(2, composeTestRule.onAllNodesWithTag("admin-custody-hosting-row-0").fetchSemanticsNodes().size
            + composeTestRule.onAllNodesWithTag("admin-custody-hosting-row-1").fetchSemanticsNodes().size)
        composeTestRule.onNodeWithTag("admin-custody-hosting-host-0").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-owner-0").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-url-0").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-budget-0").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-held-0").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-stopped-0").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-receipt-0").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-0").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-host-1").assertExists()
    }

    @Test
    fun aCaplessRowReadsAsDefaultNeverAsZeroBytes() {
        // 0 means the row carries no cap and the pump substitutes the
        // hard-coded default — the leg must render Default, never "0 B".
        render(rows = listOf(row(retainedBytesCap = 0)))
        val budget = composeTestRule.onNodeWithTag("admin-custody-hosting-budget-0")
        budget.assertTextEquals("Default")
    }

    @Test
    fun stoppedAndActiveRenderDistinctWords() {
        render(rows = listOf(row(stopped = true), row(stopped = false)))
        composeTestRule.onNodeWithTag("admin-custody-hosting-stopped-0").assertTextEquals("Paused")
        composeTestRule.onNodeWithTag("admin-custody-hosting-stopped-1").assertTextEquals("Active")
    }

    @Test
    fun everyReceiptStateGetsItsOwnWord() {
        render(
            rows = listOf(
                row(receiptState = FfiReceiptState.FRESH),
                row(receiptState = FfiReceiptState.STALE),
                row(receiptState = FfiReceiptState.NO_RECEIPT_YET),
            ),
        )
        val words = (0..2).map {
            composeTestRule.onNodeWithTag("admin-custody-hosting-receipt-$it").fetchSemanticsNode()
                .config[androidx.compose.ui.semantics.SemanticsProperties.Text].joinToString { t -> t.text }
        }
        assertEquals(3, words.toSet().size)
    }

    @Test
    fun theArmedConfirmBelongsToExactlyOneRow() {
        // Opening row 1's confirm must not disturb row 0: row 0's remove
        // button stays visible/actionable, and exactly one confirm pair is
        // painted (mirrors tui's the_armed_confirm_belongs_to_exactly_one_row).
        render(
            rows = listOf(
                row(host = "aa".repeat(32), grant = byteArrayOf(1)),
                row(host = "bb".repeat(32), grant = byteArrayOf(2)),
            ),
        )
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-1").performScrollTo().performClick()

        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-0").assertExists()
        composeTestRule.onAllNodesWithTag("admin-custody-hosting-remove-button-1").assertCountEquals(0)
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-confirm-button").assertExists()
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-cancel-button").assertExists()
    }

    @Test
    fun cancelDisarmsWithoutRemoving() {
        var removed: Pair<String, ByteArray>? = null
        render(rows = listOf(row(host = "aa".repeat(32))), onRemove = { h, g -> removed = h to g })
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-0").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-cancel-button").performScrollTo().performClick()
        assertNull(removed)
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-0").assertExists()
    }

    @Test
    fun confirmFiresOnRemoveWithTheRowsHostAndGrant() {
        var removed: Pair<String, ByteArray>? = null
        val host = "cc".repeat(32)
        val grant = byteArrayOf(9, 9)
        render(rows = listOf(row(host = host, grant = grant)), onRemove = { h, g -> removed = h to g })
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-button-0").performScrollTo().performClick()
        composeTestRule.onNodeWithTag("admin-custody-hosting-remove-confirm-button").performScrollTo().performClick()
        assertEquals(host, removed?.first)
        assertEquals(grant.toList(), removed?.second?.toList())
    }
}
