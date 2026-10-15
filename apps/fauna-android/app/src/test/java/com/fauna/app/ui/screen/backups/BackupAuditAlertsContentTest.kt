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
 * § Audit-alert surface) and the recovery gesture on the fifth reason. Which
 * verdicts are loud, and which banner offers the recovery, are decided in
 * shared Rust before this composable ever sees a banner; this only proves the
 * render is silent when healthy, indexed when not, and that the gesture's
 * states (offer, confirm, cancel, a run that disarms every button, result)
 * paint as `backups.md` specifies — mirroring tui's nest-leg set.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class BackupAuditAlertsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun banner(text: String, id: String = "id-a", offer: Boolean = false) =
        AuditBanner(text = text, destinationId = id, offerRecover = offer)

    @Test
    fun noAlerts_rendersNoBanners() {
        composeTestRule.setContent { BackupAuditAlertsContent(emptyList()) }
        composeTestRule.onAllNodesWithTag("backup-audit-alert").assertCountEquals(0)
    }

    @Test
    fun alerts_renderIndexedBanners_withText() {
        composeTestRule.setContent {
            BackupAuditAlertsContent(
                listOf(banner("Aunt's nest is 3 days behind"), banner("Home box is unreachable")),
            )
        }
        composeTestRule.onAllNodesWithTag("backup-audit-alert").assertCountEquals(2)
        composeTestRule.onAllNodesWithTag("backup-audit-alert")[0]
            .assertTextContains("Aunt's nest is 3 days behind", substring = true)
        composeTestRule.onAllNodesWithTag("backup-audit-alert")[1]
            .assertTextContains("Home box is unreachable", substring = true)
    }

    @Test
    fun anOfferedBanner_carriesTheRecoverButton_andNoOtherDoes() {
        composeTestRule.setContent {
            BackupAuditAlertsContent(
                listOf(banner("Regressed", id = "nest", offer = true), banner("Behind", id = "other")),
            )
        }
        composeTestRule.onAllNodesWithTag("backup-audit-recover-button").assertCountEquals(1)
    }

    @Test
    fun anOwnClientDeviceBanner_carriesTheRecoverButton_andAnotherDevicesDoesNot() {
        // The caller resolves `offerRecover` from the shared offered-rule
        // (`recoverOffered` on the audit row) alone: it is true for a nest and for
        // this device's own custodian row, false for another device's row, so the
        // content paints exactly the offered banners.
        composeTestRule.setContent {
            BackupAuditAlertsContent(
                listOf(
                    banner("Own copy regressed", id = "own-device", offer = true),
                    banner("Other copy regressed", id = "other-device"),
                ),
            )
        }
        composeTestRule.onAllNodesWithTag("backup-audit-recover-button").assertCountEquals(1)
    }

    @Test
    fun tappingTheButton_reportsItsDestination() {
        var armed: String? = null
        composeTestRule.setContent {
            BackupAuditAlertsContent(
                listOf(banner("Regressed", id = "nest", offer = true)),
                onRecover = { armed = it },
            )
        }
        composeTestRule.onNodeWithTag("backup-audit-recover-button").performClick()
        assert(armed == "nest")
    }

    @Test
    fun confirmingShowsTheModal_withConfirmAndCancel() {
        composeTestRule.setContent {
            BackupAuditAlertsContent(
                listOf(banner("Regressed", offer = true)),
                recoverConfirming = true,
            )
        }
        composeTestRule.onNodeWithTag("backup-audit-recover-confirm-modal").assertExists()
        composeTestRule.onNodeWithTag("backup-audit-recover-confirm-button").assertExists()
        composeTestRule.onNodeWithTag("backup-audit-recover-cancel-button").assertExists()
    }

    @Test
    fun cancel_callsBackWithoutConfirming() {
        var confirmed = 0
        var cancelled = 0
        composeTestRule.setContent {
            BackupAuditAlertsContent(
                listOf(banner("Regressed", offer = true)),
                recoverConfirming = true,
                onRecoverConfirm = { confirmed++ },
                onRecoverCancel = { cancelled++ },
            )
        }
        composeTestRule.onNodeWithTag("backup-audit-recover-cancel-button").performClick()
        assert(cancelled == 1 && confirmed == 0)
    }

    @Test
    fun confirm_callsBackOnce() {
        var confirmed = 0
        composeTestRule.setContent {
            BackupAuditAlertsContent(
                listOf(banner("Regressed", offer = true)),
                recoverConfirming = true,
                onRecoverConfirm = { confirmed++ },
            )
        }
        composeTestRule.onNodeWithTag("backup-audit-recover-confirm-button").performClick()
        assert(confirmed == 1)
    }

    @Test
    fun aRunInFlight_disarmsEveryRecoverButton_andShowsTheRunningLine() {
        composeTestRule.setContent {
            BackupAuditAlertsContent(
                listOf(
                    banner("Regressed A", id = "a", offer = true),
                    banner("Regressed B", id = "b", offer = true),
                ),
                recoverEnabled = false,
                recoverResultText = "Recovering what your nest lost…",
            )
        }
        composeTestRule.onAllNodesWithTag("backup-audit-recover-button")[0].assertIsNotEnabled()
        composeTestRule.onAllNodesWithTag("backup-audit-recover-button")[1].assertIsNotEnabled()
        composeTestRule.onNodeWithTag("backup-audit-recover-result")
            .assertTextContains("Recovering", substring = true)
    }

    @Test
    fun theResult_rendersTheResolvedLines() {
        composeTestRule.setContent {
            BackupAuditAlertsContent(
                emptyList(),
                recoverResultText = "Recovered everything\nPhotos: 4 restored",
            )
        }
        composeTestRule.onNodeWithTag("backup-audit-recover-result")
            .assertTextContains("Photos: 4 restored", substring = true)
    }

    @Test
    fun noModalNoResult_whenIdle() {
        composeTestRule.setContent {
            BackupAuditAlertsContent(listOf(banner("Regressed", offer = true)))
        }
        composeTestRule.onAllNodesWithTag("backup-audit-recover-confirm-modal").assertCountEquals(0)
        composeTestRule.onAllNodesWithTag("backup-audit-recover-result").assertCountEquals(0)
    }
}
