package com.fauna.app.ui.screen.notifications

import android.content.Context
import androidx.compose.ui.test.assertIsEnabled
import androidx.compose.ui.test.assertIsNotEnabled
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.onNodeWithText
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.ApiClient
import com.fauna.app.core.SecureStorage
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.app.ui.viewmodel.NotificationsVM
import com.fauna.ffi.FfiNotifItem
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility
import org.mockito.Mockito.`when` as whenever

/**
 * The notifications page-wide actions wait on the first list read
 * (`notifications.md` § Errors & edge cases; `docs/goal/ui/README.md` § Offer
 * only what is possible, rules 2, 5 and 6; tui's
 * `the_page_wide_actions_wait_for_the_first_read`): before it both are blocked
 * still loading and their presses refuse with that reason on `error-message`,
 * sending nothing; a first read that threw blocks them load-failed; once read,
 * an empty page blocks *Clear all* with `notifications.nothing_to_clear`. The
 * answers are the live shared predicates over UniFFI (`just android-host-test`).
 */
@ExperimentalCoroutinesApi
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class NotificationsPageActionsTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val context: Context = ApplicationProvider.getApplicationContext()
    private val api: ApiClient = mock(ApiClient::class.java)

    private fun vm(): NotificationsVM {
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.notificationTick).thenReturn(MutableSharedFlow())
        return NotificationsVM(api, mock(SecureStorage::class.java), context)
    }

    private fun item() = FfiNotifItem(
        id = 1L,
        notifType = "like",
        source = "native",
        senderId = null,
        contentId = null,
        subjectUri = null,
        summary = "A liked your post",
        isRead = false,
        createdAt = 100L,
    )

    private fun blocked(key: String) = Possibility.Blocked(LocalizedText(key, emptyMap()))

    private fun nestCalls(): List<String> =
        mockingDetails(api).invocations.map { it.method.name }
            .filter { it == "markNotificationsRead" || it == "clearNotifications" }

    @Test
    fun beforeTheFirstReadBothActionsAreBlockedAndRefuse() {
        val vm = vm()
        assertEquals(blocked("common.still_loading"), vm.markAllReadAnswer.value)
        assertEquals(blocked("common.still_loading"), vm.clearAllAnswer.value)
        vm.markAllRead()
        assertEquals("Still loading — try again in a moment", vm.errorMessage.value)
        vm.errorMessage.value = null
        vm.clearAll()
        assertEquals("Still loading — try again in a moment", vm.errorMessage.value)
        assertEquals(emptyList<String>(), nestCalls())
    }

    @Test
    fun aFailedFirstReadBlocksBothWithLoadFailed() {
        val vm = vm()
        vm.applyRead(null)
        assertEquals(blocked("common.load_failed"), vm.markAllReadAnswer.value)
        assertEquals(blocked("common.load_failed"), vm.clearAllAnswer.value)
        vm.markAllRead()
        assertEquals("Failed to load", vm.errorMessage.value)
        assertEquals(emptyList<String>(), nestCalls())
    }

    @Test
    fun onceReadAnEmptyPageHasNothingToClearAndARowEnablesIt() {
        val vm = vm()
        vm.applyRead(emptyList())
        assertEquals(Possibility.Possible, vm.markAllReadAnswer.value)
        assertEquals(blocked("notifications.nothing_to_clear"), vm.clearAllAnswer.value)
        vm.clearAll()
        assertEquals("No notifications to clear", vm.errorMessage.value)

        vm.applyRead(listOf(item()))
        assertEquals(Possibility.Possible, vm.clearAllAnswer.value)
        // A later failed read keeps the rows already shown actionable.
        vm.applyRead(null)
        assertEquals(Possibility.Possible, vm.markAllReadAnswer.value)
        assertEquals(Possibility.Possible, vm.clearAllAnswer.value)
    }

    @Test
    fun theButtonsPaintTheAnswersWithTheirReasons() {
        var presses = 0
        composeTestRule.setContent {
            NotificationsPageActions(
                markAllReadAnswer = blocked("common.still_loading"),
                clearAllAnswer = blocked("notifications.nothing_to_clear"),
                onMarkAllRead = { presses++ },
                onClearAll = { presses++ },
            )
        }
        composeTestRule.onNodeWithTag("notification-mark-read").assertIsNotEnabled()
        composeTestRule.onNodeWithTag("notification-clear").assertIsNotEnabled()
        composeTestRule.onNodeWithText("Still loading — try again in a moment").assertExists()
        composeTestRule.onNodeWithText("No notifications to clear").assertExists()
        composeTestRule.onNodeWithTag("notification-mark-read").performClick()
        assertEquals(0, presses)
    }

    @Test
    fun possibleAnswersLeaveTheButtonsLive() {
        composeTestRule.setContent {
            NotificationsPageActions(
                markAllReadAnswer = Possibility.Possible,
                clearAllAnswer = Possibility.Possible,
                onMarkAllRead = {},
                onClearAll = {},
            )
        }
        composeTestRule.onNodeWithTag("notification-mark-read").assertIsEnabled()
        composeTestRule.onNodeWithTag("notification-clear").assertIsEnabled()
    }
}
