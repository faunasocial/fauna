package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [AdminCalendarContent] (the flat
 * `admin-calendar` page, admin.md § 8 Calendar): the deployment-wide CalDAV-enable
 * toggle + the admin-set CalDAV port field. Renders with seeded state — no Hilt, no
 * VM, no FFI native calls. Verifies the heading + toggle + port field render, a
 * toggle click dispatches the flipped value, the toggle is disabled while a write is
 * in flight, and the port save button validates `[1, 65535]` (valid → onSavePort,
 * out-of-range / empty → onInvalidPort).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminCalendarContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        caldavEnabled: Boolean = false,
        caldavPort: Int = 8443,
        working: Boolean = false,
        onSetCaldavEnabled: (Boolean) -> Unit = {},
        onSavePort: (Int) -> Unit = {},
        onInvalidPort: () -> Unit = {},
    ) {
        composeTestRule.setContent {
            AdminCalendarContent(
                caldavEnabled = caldavEnabled,
                caldavPort = caldavPort,
                working = working,
                onBack = {},
                onSetCaldavEnabled = onSetCaldavEnabled,
                onSavePort = onSavePort,
                onInvalidPort = onInvalidPort,
                // FFI-free stub of the shared `parse_port` (range logic is unit-tested
                // in shared Rust); keeps this content-test off the native FFI path.
                parsePort = { it.toIntOrNull()?.takeIf { p -> p in 1..65535 } },
            )
        }
    }

    @Test
    fun rendersHeadingToggleAndPortField() {
        render()
        composeTestRule.onNodeWithTag("admin-calendar-heading").assertExists()
        composeTestRule.onNodeWithTag("admin-nav-back").assertExists()
        composeTestRule.onNodeWithTag("admin-calendar-enabled-toggle").assertExists()
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-input").assertExists()
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-save-button").assertExists()
    }

    @Test
    fun toggleDispatchesFlippedValue() {
        var set: Boolean? = null
        render(caldavEnabled = false, onSetCaldavEnabled = { set = it })
        composeTestRule.onNodeWithTag("admin-calendar-enabled-toggle").performClick()
        assertEquals(true, set)
    }

    @Test
    fun toggleDisabledWhileWorking() {
        render(working = true)
        composeTestRule.onNodeWithTag("admin-calendar-enabled-toggle").assertIsNotEnabled()
    }

    @Test
    fun savePortDispatchesEditedValue() {
        var saved: Int? = null
        var invalid = false
        render(caldavPort = 8443, onSavePort = { saved = it }, onInvalidPort = { invalid = true })
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-input")
            .performTextReplacement("9443")
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-save-button").performClick()
        assertEquals(9443, saved)
        assertEquals(false, invalid)
    }

    @Test
    fun savePortRejectsOutOfRange() {
        var saved: Int? = null
        var invalid = false
        render(caldavPort = 8443, onSavePort = { saved = it }, onInvalidPort = { invalid = true })
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-input")
            .performTextReplacement("70000")
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-save-button").performClick()
        assertEquals(null, saved)
        assertEquals(true, invalid)
    }

    @Test
    fun savePortRejectsEmpty() {
        var saved: Int? = null
        var invalid = false
        render(caldavPort = 8443, onSavePort = { saved = it }, onInvalidPort = { invalid = true })
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-input")
            .performTextReplacement("")
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-save-button").performClick()
        assertEquals(null, saved)
        assertEquals(true, invalid)
    }

    @Test
    fun portFieldDisabledWhileWorking() {
        render(working = true)
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-input").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("admin-calendar-caldav-port-save-button").assertIsNotEnabled()
    }
}
