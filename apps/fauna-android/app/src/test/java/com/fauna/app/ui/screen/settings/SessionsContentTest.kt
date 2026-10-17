package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.remember
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.assertTextEquals
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.compose.ui.test.performScrollTo
import androidx.compose.ui.test.performTextInput
import com.fauna.app.R
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.viewmodel.SessionsUiState
import com.fauna.ffi.FfiSessionRow
import com.fauna.ffi.FfiSessionsSnapshot
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_core.LocalizedText

/**
 * Settings → Sessions (`ui/sessions.md`): the render paints the shared fold's
 * rows verbatim — no revoke on this app's own row — and owns only the two
 * confirm states, with the required copy always visible.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class SessionsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun row(id: String, own: Boolean) = FfiSessionRow(
        tokenId = id,
        kind = LocalizedText(key = "sessions.kind_app", args = emptyMap()),
        mark = if (own) LocalizedText(key = "sessions.mark_this_app", args = emptyMap()) else null,
        detail = LocalizedText(key = "sessions.address_not_recorded", args = emptyMap()),
        createdAt = 1u, lastUsedAt = 2u, expiresAt = 3u, ipAddress = null,
        canRevoke = !own,
    )

    private val two = FfiSessionsSnapshot(rows = listOf(row("own", true), row("other", false)), error = null)

    @Test
    fun theOwnRowHasNoRevokeAndTheOtherRowRevokesItsToken() {
        val revoked = mutableListOf<String>()
        composeTestRule.setContent {
            SessionsContent(
                state = SessionsUiState(snapshot = two),
                lockConfirmWord = "LOCK",
                onBack = {}, onRevoke = { revoked += it },
                onArmRevokeOthers = {}, onCancelRevokeOthers = {}, onConfirmRevokeOthers = {},
                onLockWordChange = {}, onLock = {},
            )
        }
        composeTestRule.onAllNodesWithTag(Ids.SESSION_CARD).assertCountEquals(2)
        composeTestRule.onAllNodesWithTag(Ids.SESSION_THIS_MARK_BADGE).assertCountEquals(1)
        composeTestRule.onAllNodesWithTag(Ids.SESSION_REVOKE_BUTTON).assertCountEquals(1)
        composeTestRule.onNodeWithTag(Ids.SESSION_REVOKE_BUTTON).performClick()
        assertEquals(listOf("other"), revoked)
        composeTestRule.onNodeWithTag(Ids.SESSIONS_EMPTY).assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.SESSIONS_REVOKE_NOTE)
            .assertTextEquals(getString(R.string.sessions_revoke_note))
        composeTestRule.onNodeWithTag(Ids.SESSIONS_LOCKOUT_WARNING)
            .assertTextEquals(getString(R.string.sessions_lockout_warning))
    }

    @Test
    fun signOutEverywhereElseIsATwoPressInlineConfirm() {
        var confirmed = 0
        composeTestRule.setContent {
            var armed by remember { mutableStateOf(false) }
            SessionsContent(
                state = SessionsUiState(snapshot = two, revokeOthersArmed = armed),
                lockConfirmWord = "LOCK",
                onBack = {}, onRevoke = {},
                onArmRevokeOthers = { armed = true },
                onCancelRevokeOthers = { armed = false },
                onConfirmRevokeOthers = { confirmed++; armed = false },
                onLockWordChange = {}, onLock = {},
            )
        }
        composeTestRule.onNodeWithTag(Ids.SESSIONS_REVOKE_OTHERS_CONFIRM_BUTTON).assertDoesNotExist()
        composeTestRule.onNodeWithTag(Ids.SESSIONS_REVOKE_OTHERS_BUTTON).performClick()
        composeTestRule.onNodeWithTag(Ids.SESSIONS_REVOKE_OTHERS_CANCEL_BUTTON).performClick()
        assertEquals(0, confirmed)
        composeTestRule.onNodeWithTag(Ids.SESSIONS_REVOKE_OTHERS_BUTTON).performClick()
        composeTestRule.onNodeWithTag(Ids.SESSIONS_REVOKE_OTHERS_CONFIRM_BUTTON).performClick()
        assertEquals(1, confirmed)
    }

    @Test
    fun theLockArmsOnlyOnTheTypedWord() {
        var locked = 0
        composeTestRule.setContent {
            var word by remember { mutableStateOf("") }
            SessionsContent(
                state = SessionsUiState(snapshot = two, lockWord = word),
                lockConfirmWord = "LOCK",
                onBack = {}, onRevoke = {},
                onArmRevokeOthers = {}, onCancelRevokeOthers = {}, onConfirmRevokeOthers = {},
                onLockWordChange = { word = it }, onLock = { locked++ },
            )
        }
        composeTestRule.onNodeWithTag(Ids.SESSIONS_LOCKOUT_BUTTON).assertIsNotEnabled()
        composeTestRule.onNodeWithTag(Ids.SESSIONS_LOCKOUT_CONFIRM_FIELD).performTextInput("LOCK")
        composeTestRule.onNodeWithTag(Ids.SESSIONS_LOCKOUT_BUTTON).performScrollTo().assertIsEnabled().performClick()
        assertEquals(1, locked)
    }

    @Test
    fun emptyPaintsOnlyAfterARead() {
        var state by mutableStateOf(SessionsUiState())
        composeTestRule.setContent {
            SessionsContent(
                state = state,
                lockConfirmWord = "LOCK",
                onBack = {}, onRevoke = {},
                onArmRevokeOthers = {}, onCancelRevokeOthers = {}, onConfirmRevokeOthers = {},
                onLockWordChange = {}, onLock = {},
            )
        }
        composeTestRule.onNodeWithTag(Ids.SESSIONS_EMPTY).assertDoesNotExist()
        state = SessionsUiState(snapshot = FfiSessionsSnapshot(rows = emptyList(), error = null))
        composeTestRule.onNodeWithTag(Ids.SESSIONS_EMPTY).assertExists()
    }

    private fun getString(resId: Int): String =
        androidx.test.core.app.ApplicationProvider.getApplicationContext<android.content.Context>()
            .getString(resId)
}
