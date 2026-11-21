package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.CompositionLocalProvider
import androidx.compose.ui.platform.LocalClipboardManager
import androidx.compose.ui.semantics.SemanticsProperties
import androidx.compose.ui.test.SemanticsMatcher
import androidx.compose.ui.test.assert
import androidx.compose.ui.test.assertIsDisplayed
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.hasText
import com.fauna.app.ui.FailingClipboard
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
        onError: (String) -> Unit = {},
        failingClipboard: Boolean = false,
    ) {
        composeTestRule.setContent {
            val page = @androidx.compose.runtime.Composable {
                SettingsLogsContent(
                    rows = rows,
                    copyText = copyText,
                    filterIndex = filterIndex,
                    onFilterChange = onFilterChange,
                    onClear = onClear,
                    onBack = {},
                    onError = onError,
                )
            }
            if (failingClipboard) {
                CompositionLocalProvider(LocalClipboardManager provides FailingClipboard("no clipboard here")) { page() }
            } else {
                page()
            }
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

    @Test
    fun emptyRingDisablesCopyAndClearBehindThePagesEmptyLine() {
        // `ring_action_possibility(0, "logs.empty")` is blocked: both buttons
        // disabled, the reason being the empty line already on the page (painted
        // once — no second copy beside the buttons).
        var cleared = false
        render(rows = emptyList(), copyText = "", onClear = { cleared = true })
        composeTestRule.onNodeWithTag("log-copy-button").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("log-clear-button").assertIsNotEnabled()
        assertEquals(
            1,
            composeTestRule.onAllNodes(hasText("No log entries yet.")).fetchSemanticsNodes().size,
        )
        assertEquals(false, cleared)
    }

    @Test
    fun shownEntriesEnableCopyAndClear() {
        render()
        composeTestRule.onNodeWithTag("log-copy-button").assertIsEnabled()
        composeTestRule.onNodeWithTag("log-clear-button").assertIsEnabled()
    }

    @Test
    fun aCopyThatReachedTheClipboardIsConfirmed() {
        render()
        composeTestRule.onNodeWithTag("log-copy-button").performClick()
        // "Copied!" (`common.copied`) on the button, the copied block as its
        // `copied` attr — the `account-actor-id-copy-btn` contract.
        composeTestRule.onNodeWithTag("log-copy-button").assert(hasText("Copied!"))
        composeTestRule.onNodeWithTag("log-copy-button")
            .assert(SemanticsMatcher.expectValue(SemanticsProperties.StateDescription, copyPayload))
    }

    @Test
    fun aFailedCopySaysSoOnTheErrorMessage() {
        var error: String? = null
        render(onError = { error = it }, failingClipboard = true)
        composeTestRule.onNodeWithTag("log-copy-button").performClick()
        assertEquals("Could not copy to the clipboard: no clipboard here", error)
        // Not confirmed.
        assertEquals(0, composeTestRule.onAllNodes(hasText("Copied!")).fetchSemanticsNodes().size)
    }
}
