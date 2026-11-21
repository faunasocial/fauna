package com.fauna.app.ui.screen.settings

import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.Possibility

/**
 * The admin CardDAV (`admin-contacts`) and WebDAV (`admin-files`) enable
 * toggles wait for `get_mail_config` (admin.md § Errors & edge cases,
 * *Controls wait for their read*): painted from the shared
 * `toggle_possibility` — blocked with its reason, published as the toggle's
 * automation `reason` — and a press while blocked writes nothing. The
 * calendar sibling is covered in [AdminCalendarContentTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class AdminDavTogglesContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun contacts(answer: Possibility, onSet: (Boolean) -> Unit = {}) {
        composeTestRule.setContent {
            AdminContactsContent(
                carddavEnabled = false,
                working = false,
                onBack = {},
                onSetCarddavEnabled = onSet,
                toggleAnswer = answer,
            )
        }
    }

    private fun files(answer: Possibility, onSet: (Boolean) -> Unit = {}) {
        composeTestRule.setContent {
            AdminFilesContent(
                webdavEnabled = false,
                working = false,
                onBack = {},
                onSetWebdavEnabled = onSet,
                toggleAnswer = answer,
            )
        }
    }

    @Test
    fun carddavToggleWaitsForTheRead() {
        var set: Boolean? = null
        contacts(ReadGateFakes.stillLoading) { set = it }
        composeTestRule.onNodeWithTag("admin-contacts-carddav-enabled-toggle")
            .assertIsNotEnabled()
            .assertPaintedReason(ReadGateFakes.stillLoadingText)
            .performClick()
        assertEquals(null, set)
    }

    @Test
    fun carddavToggleSaysFailedAfterAFailedRead() {
        contacts(ReadGateFakes.loadFailed)
        composeTestRule.onNodeWithTag("admin-contacts-carddav-enabled-toggle")
            .assertIsNotEnabled()
            .assertPaintedReason(ReadGateFakes.loadFailedText)
    }

    @Test
    fun carddavToggleLiveOnceRead() {
        var set: Boolean? = null
        contacts(Possibility.Possible) { set = it }
        composeTestRule.onNodeWithTag("admin-contacts-carddav-enabled-toggle")
            .assertIsEnabled()
            .assertNoPaintedReason()
            .performClick()
        assertEquals(true, set)
    }

    @Test
    fun webdavToggleWaitsForTheRead() {
        var set: Boolean? = null
        files(ReadGateFakes.stillLoading) { set = it }
        composeTestRule.onNodeWithTag("admin-files-webdav-enabled-toggle")
            .assertIsNotEnabled()
            .assertPaintedReason(ReadGateFakes.stillLoadingText)
            .performClick()
        assertEquals(null, set)
    }

    @Test
    fun webdavToggleSaysSavingInFlight() {
        files(ReadGateFakes.inFlight)
        composeTestRule.onNodeWithTag("admin-files-webdav-enabled-toggle")
            .assertIsNotEnabled()
            .assertPaintedReason(ReadGateFakes.inFlightText)
    }
}
