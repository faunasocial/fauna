package com.fauna.app.ui.screen.folders

import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [PhotoBackupControlsContent] — the
 * `photo-backup-controls` component (`docs/goal/ui/folders.md` § Photo backup),
 * rendered as a section on Settings → Folders after the 2026-07-19
 * UI-restructuring lift retired the standalone `settings/photo-backup` route.
 * Mirrors apple's shared FaunaKit `PhotoBackupControlsView` shape (priority #1).
 * FFI-free/Hilt-free — [PhotoBackupSection] wires the real VM + permission
 * launcher (android E2E stays host-emulator-gated).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class PhotoBackupControlsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun render(
        hasPermission: Boolean = true,
        autoBackup: Boolean = false,
        isSyncing: Boolean = false,
        syncedCount: Int = 0,
        pendingCount: Int = 0,
        lastError: String? = null,
        actions: PhotoBackupActions = PhotoBackupActions(),
    ) {
        composeTestRule.setContent {
            PhotoBackupControlsContent(
                hasPermission = hasPermission,
                autoBackup = autoBackup,
                isSyncing = isSyncing,
                syncedCount = syncedCount,
                pendingCount = pendingCount,
                lastError = lastError,
                actions = actions,
            )
        }
    }

    @Test
    fun enableToggleAlwaysRendersEvenWhenOff() {
        render(autoBackup = false)
        composeTestRule.onNodeWithTag("photo-backup-enable-toggle").assertExists().assertIsOff()
        // The rest of the surface stays hidden until enabled.
        composeTestRule.onNodeWithTag("photo-backup-wifi-only-toggle").assertDoesNotExist()
        composeTestRule.onNodeWithTag("photo-backup-sync-now-button").assertDoesNotExist()
    }

    @Test
    fun enableToggleFiresCallback() {
        var enabled: Boolean? = null
        render(actions = PhotoBackupActions(onSetAutoBackup = { enabled = it }))
        composeTestRule.onNodeWithTag("photo-backup-enable-toggle").performClick()
        assertEquals(true, enabled)
    }

    @Test
    fun missingPermissionShowsRequiredMessageAndHidesControls() {
        render(autoBackup = true, hasPermission = false)
        composeTestRule.onNodeWithText("Photo library access required. Grant access in Settings.").assertExists()
        composeTestRule.onNodeWithTag("photo-backup-wifi-only-toggle").assertDoesNotExist()
        composeTestRule.onNodeWithTag("photo-backup-sync-now-button").assertDoesNotExist()
    }

    @Test
    fun enabledWithPermissionRendersFullControls() {
        render(autoBackup = true, hasPermission = true, syncedCount = 5, pendingCount = 2)
        composeTestRule.onNodeWithTag("photo-backup-wifi-only-toggle").assertExists().assertIsOn().assertIsNotEnabled()
        composeTestRule.onNodeWithTag("photo-backup-last-sync-text").assertExists()
        composeTestRule.onNodeWithTag("photo-backup-sync-now-button").assertExists().assertIsEnabled()
        composeTestRule.onNodeWithText("5 photos").assertExists()
        composeTestRule.onNodeWithText("2 pending").assertExists()
    }

    @Test
    fun syncNowButtonFiresCallback() {
        var fired = false
        render(autoBackup = true, actions = PhotoBackupActions(onSyncNow = { fired = true }))
        composeTestRule.onNodeWithTag("photo-backup-sync-now-button").performClick()
        assertTrue(fired)
    }

    @Test
    fun syncProgressShowsOnlyWhileSyncing() {
        render(autoBackup = true, isSyncing = true)
        composeTestRule.onNodeWithTag("photo-backup-sync-progress").assertExists()
        composeTestRule.onNodeWithTag("photo-backup-sync-now-button").assertIsNotEnabled()
    }

    @Test
    fun syncProgressHiddenWhenNotSyncing() {
        render(autoBackup = true, isSyncing = false)
        composeTestRule.onNodeWithTag("photo-backup-sync-progress").assertDoesNotExist()
        composeTestRule.onNodeWithTag("photo-backup-sync-now-button").assertIsEnabled()
    }

    @Test
    fun errorRendersWhenPresent() {
        render(autoBackup = true, lastError = "backup failed")
        composeTestRule.onNodeWithText("backup failed").assertExists()
    }

    @Test
    fun pendingRowHiddenWhenZero() {
        render(autoBackup = true, syncedCount = 3, pendingCount = 0)
        composeTestRule.onNodeWithText("0 pending").assertDoesNotExist()
    }
}
