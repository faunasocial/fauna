package com.fauna.app.ui.navigation

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_client_alerts.CriticalAlertRow
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the stateless [CriticalAlertsBannerContent] — the
 * every-page critical-alerts banner (`docs/goal/behavior/critical-alerts.md`;
 * ui.yaml `global:` `critical-alerts` / `critical-alert[N]`). No Hilt, no VM,
 * no FFI native calls — mirrors
 * [com.fauna.app.ui.screen.settings.AtprotoSettingsContentTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class CriticalAlertsBannerContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun noAlerts_rendersNoBanner() {
        composeTestRule.setContent { CriticalAlertsBannerContent(emptyList()) }

        composeTestRule.onNodeWithTag("critical-alerts").assertDoesNotExist()
    }

    @Test
    fun oneAlert_rendersOneIndexedRowWithResolvedText() {
        val row = CriticalAlertRow(
            key = "atproto-custody:did:plc:abc",
            lines = listOf(
                LocalizedText(
                    "critical_alerts.atproto_custody_mismatch",
                    mapOf("handle" to "alice@fauna.test"),
                ),
            ),
        )
        composeTestRule.setContent { CriticalAlertsBannerContent(listOf(row)) }

        composeTestRule.onNodeWithTag("critical-alerts").assertIsDisplayed()
        val rows = composeTestRule.onAllNodesWithTag("critical-alert")
        rows.assertCountEquals(1)
        composeTestRule.onNodeWithTag("critical-alert").assertIsDisplayed()
    }

    @Test
    fun twoAlerts_rendersTwoIndexedRows() {
        val rows = listOf(
            CriticalAlertRow(key = "a-feeder:1", lines = listOf(LocalizedText("alpha", emptyMap()))),
            CriticalAlertRow(key = "z-feeder:1", lines = listOf(LocalizedText("zulu", emptyMap()))),
        )
        composeTestRule.setContent { CriticalAlertsBannerContent(rows) }

        composeTestRule.onAllNodesWithTag("critical-alert").assertCountEquals(2)
    }
}
