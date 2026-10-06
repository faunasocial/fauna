package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.ui.currentClipboardText
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_log.LogRow

/**
 * Compose-level coverage for the stateless [SettingsLogsContent] (the client's
 * Settings → Logs sub-page, observability.md § Surfaces). The severity filter +
 * row form now live in shared Rust (`fauna_log::format`, via the `com.fauna.ffi.log_*`
 * exports the stateful screen calls); this Content faithfully renders the
 * pre-computed [LogRow]s + copy payload, so the test seeds them — no Hilt, no VM,
 * no native FFI — and exercises the page landmark, the canonical ui.yaml component
 * IDs (`log-entry` / `log-level-filter` / `log-copy-button` / `log-clear-button`),
 * the row count, and the copy/clear callbacks. (Android E2E `test_settings_logs.py
 * --client android` is the standing gate once the host emulator lands.)
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class SettingsLogsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun row(message: String) = LogRow(
        line = "ERROR · 12:00:00 · fauna_test · $message",
        message = message,
        subtitle = "ERROR · 12:00:00 · fauna_test",
    )

    private val rows = listOf(row("boom"), row("started"), row("detail"))
    private val copyPayload = rows.joinToString("\n") { it.line }

    private fun render(
        rows: List<LogRow> = this.rows,
        copyText: String = copyPayload,
        filterIndex: Int = 0,
        onFilterChange: (Int) -> Unit = {},
        onClear: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            SettingsLogsContent(
                rows = rows,
                copyText = copyText,
                filterIndex = filterIndex,
                onFilterChange = onFilterChange,
                onClear = onClear,
                onBack = {},
            )
        }
    }

    @Test
    fun rendersLandmarkAndControls() {
        render()
        composeTestRule.onNodeWithTag("settings-logs").assertExists()
        composeTestRule.onNodeWithTag("log-level-filter").assertExists()
        composeTestRule.onNodeWithTag("log-copy-button").assertExists()
        composeTestRule.onNodeWithTag("log-clear-button").assertExists()
    }

    @Test
    fun rendersOneRowPerSharedRow() {
        render()
        assertEquals(
            3,
            composeTestRule.onAllNodesWithTag("log-entry").fetchSemanticsNodes().size,
        )
    }

    @Test
    fun emptyRingRendersNoRows() {
        render(rows = emptyList(), copyText = "")
        assertEquals(
            0,
            composeTestRule.onAllNodesWithTag("log-entry").fetchSemanticsNodes().size,
        )
        // The landmark + controls are still present on an empty page.
        composeTestRule.onNodeWithTag("settings-logs").assertExists()
    }

    @Test
    fun copyCopiesSuppliedTextAndClearFires() {
        var cleared = false
        render(onClear = { cleared = true })
        composeTestRule.onNodeWithTag("log-copy-button").assertIsDisplayed().performClick()
        composeTestRule.onNodeWithTag("log-clear-button").assertIsDisplayed().performClick()
        // The shared CopyButton copies the screen-supplied (shared-Rust-rendered,
        // newest-first) log block; clear stays a callback (it mutates the ring).
        assertEquals(copyPayload, currentClipboardText())
        assertEquals(true, cleared)
    }
}
