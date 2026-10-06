package com.fauna.app.ui.screen.backups

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [BackupAuditAlertsContent] — the
 * indexed `backup-audit-alert` banners (`docs/goal/ui/backups.md`
 * § Audit-alert surface). Which verdicts are loud is decided entirely in
 * shared Rust before this composable ever sees a string; this only proves the
 * render is silent when healthy and indexed when not (the
 * `restore-divergence-banner` idiom).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class BackupAuditAlertsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Test
    fun noAlerts_rendersNoBanners() {
        composeTestRule.setContent { BackupAuditAlertsContent(emptyList()) }
        composeTestRule.onAllNodesWithTag("backup-audit-alert").assertCountEquals(0)
    }

    @Test
    fun alerts_renderIndexedBanners_withText() {
        composeTestRule.setContent {
            BackupAuditAlertsContent(listOf("Aunt's nest is 3 days behind", "Home box is unreachable"))
        }
        composeTestRule.onAllNodesWithTag("backup-audit-alert").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("backup-audit-alert")[0]
            .assertTextContains("Aunt's nest is 3 days behind", substring = true)
        composeTestRule.onAllNodesWithTag("backup-audit-alert")[1]
            .assertTextContains("Home box is unreachable", substring = true)
    }
}
