package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.hasText
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.ui.FailingClipboard
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_log.LogRow

/**
 * Compose-level coverage for the stateless [AdminLogsContent] (the admin view of
 * the nest's `fauna-log` ring, observability.md § Surfaces). The severity filter +
 * row form now live in shared Rust (`fauna_log::format`, via the `com.fauna.ffi.log_*`
 * exports the stateful screen calls); this Content is a faithful pass-through of
 * the pre-computed [LogRow]s, so the test renders seeded rows — no Hilt, no VM, no
 * native FFI — and exercises the page heading, the shared `admin-nav-back`, the
 * reused Logs component IDs (`log-entry` / `log-level-filter` / `log-copy-button`),
 * and the deliberate **absence** of a clear button (no admin RPC to wipe the nest
 * ring). (Android E2E `test_admin_logs.py --client android` is the standing gate
 * once the host emulator lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminLogsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun row(message: String) = LogRow(
        line = "INFO · 12:00:00 · fauna_nest · $message",
        message = message,
        subtitle = "INFO · 12:00:00 · fauna_nest",
    )

    private val nestRows = listOf(row("nest started"), row("slow query"))

    private fun render(
        rows: List<LogRow> = nestRows,
        onError: (String) -> Unit = {},
        failingClipboard: Boolean = false,
    ) {
        composeTestRule.setContent {
            val page = @androidx.compose.runtime.Composable {
                AdminLogsContent(
                    rows = rows,
                    copyText = rows.joinToString("\n") { it.line },
                    filterIndex = 0,
                    onFilterChange = {},
                    onBack = {},
                    onError = onError,
                )
            }
            if (failingClipboard) {
                CompositionLocalProvider(LocalClipboardManager provides FailingClipboard("denied")) { page() }
            } else {
                page()
            }
        }
    }

    @Test
    fun rendersHeadingNavBackAndFilter() {
        render()
        composeTestRule.onNodeWithTag("admin-logs-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        composeTestRule.onNodeWithTag("log-level-filter").assertExists()
        composeTestRule.onNodeWithTag("log-copy-button").assertExists()
    }

    @Test
    fun hasNoClearButton() {
        // The admin view never clears the nest ring (no RPC) — only the client's
        // own Settings → Logs page has log-clear-button.
        render()
        assertEquals(
            0,
            composeTestRule.onAllNodesWithTag("log-clear-button").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun rendersOneRowPerSharedRow() {
        render()
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("log-entry").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun auditSectionRendersSeededRowsAndIntegrityStatus() {
        composeTestRule.setContent {
            AdminLogsContent(
                rows = nestRows,
                copyText = "",
                filterIndex = 0,
                onFilterChange = {},
                onBack = {},
                auditRows = listOf("2026-10-18 12:00  first", "2026-10-18 12:01  second"),
                auditStatus = "chain verified",
                auditMorePossibility = uniffi.fauna_core.Possibility.Possible,
            )
        }
        composeTestRule.onNodeWithTag("admin-audit-section").assertExists()
        assertEquals(
            2,
            composeTestRule.onAllNodesWithTag("admin-audit-entry").fetchSemanticsNodes().size,
        )
        composeTestRule.onNodeWithTag("admin-audit-integrity-status").assertExists()
        composeTestRule.onNodeWithTag("admin-audit-more-button").assertIsEnabled()
    }

    @Test
    fun auditMoreButtonDisabledWithoutOlderPage() {
        render()
        composeTestRule.onNodeWithTag("admin-audit-section").assertExists()
        assertEquals(
            0,
            composeTestRule.onAllNodesWithTag("admin-audit-entry").fetchSemanticsNodes().size,
        )
        composeTestRule.onNodeWithTag("admin-audit-more-button").assertIsNotEnabled()
    }

    @Test
    fun emptyRingRendersNoRows() {
        render(rows = emptyList())
        assertEquals(
            0,
            composeTestRule.onAllNodesWithTag("log-entry").fetchSemanticsNodes().size,
        )
        composeTestRule.onNodeWithTag("admin-logs-heading").assertExists()
    }

    @Test
    fun emptyRingDisablesCopyBehindThePagesEmptyLine() {
        // `ring_action_possibility(0, "admin.logs_page.empty")` — blocked, the
        // reason being the page's own empty line, painted once.
        render(rows = emptyList())
        composeTestRule.onNodeWithTag("log-copy-button").assertIsNotEnabled()
        assertEquals(
            1,
            composeTestRule.onAllNodes(hasText("No nest log entries yet.")).fetchSemanticsNodes().size,
        )
    }

    @Test
    fun aCopyThatReachedTheClipboardIsConfirmed() {
        render()
        composeTestRule.onNodeWithTag("log-copy-button").assertIsEnabled().performClick()
        composeTestRule.onNodeWithTag("log-copy-button").assert(hasText("Copied!"))
        composeTestRule.onNodeWithTag("log-copy-button").assert(
            SemanticsMatcher.expectValue(
                SemanticsProperties.StateDescription,
                nestRows.joinToString("\n") { it.line },
            ),
        )
    }

    @Test
    fun aFailedCopySaysSoOnTheErrorMessage() {
        var error: String? = null
        render(onError = { error = it }, failingClipboard = true)
        composeTestRule.onNodeWithTag("log-copy-button").performClick()
        assertEquals("Could not copy to the clipboard: denied", error)
    }
}
