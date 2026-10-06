package com.fauna.app.ui.screen.backups

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.ffi.FfiRestoreDivergenceRow
import com.fauna.ffi.FfiRestoreHistoryRow
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [RestoreHistoryContent] (the backups-page
 * read-only restore surface, backups.md §§ Restore history / Restore divergence):
 * the collapsible section, the indexed history rows with the "local snapshot"
 * source label, and the forensic divergence banner → close-only details modal.
 * Renders with seeded state — no Hilt, no VM, no FFI native calls (the timestamp
 * formatter defaults to a pure stub).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class RestoreHistoryContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun hist(
        snapshotId: Long = 42,
        kinds: String = "mail",
        source: ByteArray? = null,
    ) = FfiRestoreHistoryRow(
        id = snapshotId,
        completedAt = 100,
        snapshotId = snapshotId,
        kindsRestored = kinds,
        sourceMemberId = source,
    )

    private fun div(snapshotId: Long = 42, mua: String? = "Thunderbird") = FfiRestoreDivergenceRow(
        id = 1,
        snapshotId = snapshotId,
        observedAt = 0,
        protocol = "imap",
        collection = "INBOX",
        muaId = mua,
        clientModseq = 120,
        serverModseq = 100,
        lostEventCount = 20,
    )

    private fun render(
        history: List<FfiRestoreHistoryRow>,
        divergence: Map<Long, List<FfiRestoreDivergenceRow>> = emptyMap(),
        // FFI-free stub mirroring the shared hex_short (first 4 bytes → 8 hex chars)
        // so Robolectric stays off the native path — no host JNA needed.
        hexShort: (ByteArray) -> String = { it.take(4).joinToString("") { b -> "%02x".format(b) } },
    ) {
        composeTestRule.setContent {
            RestoreHistoryContent(
                history = history,
                divergenceBySnapshot = divergence,
                hexShort = hexShort,
            )
        }
    }

    @Test
    fun emptyHistory_sectionPresent_collapsed_noItems() {
        render(history = emptyList())
        composeTestRule.onNodeWithTag("restore-history-section").assertIsDisplayed()
        // Collapsed when empty → no list / rows rendered.
        composeTestRule.onAllNodesWithTag("restore-history-item").assertCountEquals(0)
    }

    @Test
    fun history_rendersIndexedItems_withLocalSnapshotSource() {
        render(history = listOf(hist(snapshotId = 1), hist(snapshotId = 2)))
        composeTestRule.onNodeWithTag("restore-history-list").assertIsDisplayed()
        composeTestRule.onAllNodesWithTag("restore-history-item").assertCountEquals(2)
        // source_member_id None → "local snapshot".
        composeTestRule.onAllNodesWithText("from local snapshot", substring = true)
            .assertCountEquals(2)
    }

    @Test
    fun noDivergence_noBanner() {
        render(history = listOf(hist()))
        composeTestRule.onAllNodesWithTag("restore-divergence-banner").assertCountEquals(0)
    }

    @Test
    fun divergence_showsBanner_andOpensCloseOnlyModal() {
        render(
            history = listOf(hist(snapshotId = 42)),
            divergence = mapOf(42L to listOf(div(), div(mua = null))),
        )
        composeTestRule.onNodeWithTag("restore-divergence-banner").assertIsDisplayed()
        composeTestRule.onNodeWithTag("restore-divergence-details-modal").assertDoesNotExist()

        composeTestRule.onNodeWithTag("restore-divergence-banner").performClick()
        composeTestRule.onNodeWithTag("restore-divergence-details-modal").assertIsDisplayed()
        composeTestRule.onAllNodesWithTag("restore-divergence-details-item").assertCountEquals(2)
        // Forensic: the unknown-MUA row renders the "(unknown)" placeholder.
        composeTestRule.onAllNodesWithText("(unknown)", substring = true).onFirst().assertExists()
    }
}
