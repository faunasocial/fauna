package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.junit4.createComposeRule
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

    private fun render(rows: List<LogRow> = nestRows) {
        composeTestRule.setContent {
            AdminLogsContent(
                rows = rows,
                copyText = rows.joinToString("\n") { it.line },
                filterIndex = 0,
                onFilterChange = {},
                onBack = {},
            )
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
    fun emptyRingRendersNoRows() {
        render(rows = emptyList())
        assertEquals(
            0,
            composeTestRule.onAllNodesWithTag("log-entry").fetchSemanticsNodes().size,
        )
        composeTestRule.onNodeWithTag("admin-logs-heading").assertExists()
    }
}
