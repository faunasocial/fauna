package com.fauna.app.ui.screen.notifications

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.assertCountEquals
import androidx.compose.ui.test.assertHasNoClickAction
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onAllNodesWithTag
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiNotifItem
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import social.fauna.generated.Ids
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.MutedKeyword

/**
 * The notifications page's muted-word collapse (`behavior/notifications.md`
 * § Muted words): a row whose relayed text carries a word muted at Hide paints
 * `notification-muted` + its reveal in place of the sentence and is no tap
 * target; the reveal hands the row back; Show less collapses nothing; a
 * security notice is never collapsed.
 *
 * FFI-touching (the decision is shared Rust's `notificationMutedFor`) — runs
 * green via `just android-host-test`.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class NotificationListRowTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val hide = listOf(MutedKeyword(keyword = "spoiler", weight = -1000L))
    private val showLess = listOf(MutedKeyword(keyword = "spoiler", weight = -10L))

    private fun knockRow(type: String = "knock", message: String = "big spoiler inside") = FfiNotifItem(
        id = 7L,
        notifType = type,
        source = "fauna",
        senderId = null,
        contentId = null,
        subjectUri = null,
        summary = "Someone knocked",
        isRead = false,
        createdAt = 0L,
        body = LocalizedText(key = "notifications.knock", args = mapOf("message" to message)),
    )

    @Test
    fun aRowCarryingAHideWordCollapsesUntilRevealed() {
        var revealed by mutableStateOf(false)
        var opened = 0
        composeTestRule.setContent {
            NotificationListRow(
                notification = knockRow(),
                mutedKeywords = hide,
                revealed = revealed,
                onReveal = { revealed = true },
                onOpen = { opened++ },
            )
        }

        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_MUTED).assertCountEquals(1)
        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_ITEM).assertCountEquals(0)
        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_TYPE_ICON).assertCountEquals(1)
        composeTestRule.onNodeWithTag(Ids.NOTIFICATION_MUTED).assertHasNoClickAction()

        composeTestRule.onNodeWithTag(Ids.NOTIFICATION_MUTED_REVEAL_BUTTON).performClick()

        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_MUTED).assertCountEquals(0)
        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_ITEM).assertCountEquals(1)
        composeTestRule.onNodeWithTag(Ids.NOTIFICATION_ITEM).performClick()
        assertEquals("the revealed row navigates as any other", 1, opened)
    }

    @Test
    fun showLessAndAnUnmatchedRowPaintInFull() {
        var keywords by mutableStateOf(showLess)
        composeTestRule.setContent {
            NotificationListRow(knockRow(), keywords, revealed = false, onReveal = {}, onOpen = null)
        }
        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_MUTED).assertCountEquals(0)

        keywords = listOf(MutedKeyword(keyword = "unrelated", weight = -1000L))
        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_MUTED).assertCountEquals(0)
        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_ITEM).assertCountEquals(1)
    }

    @Test
    fun aSecurityNoticeIsNeverCollapsed() {
        composeTestRule.setContent {
            NotificationListRow(
                knockRow(type = "security.notice"),
                hide,
                revealed = false,
                onReveal = {},
                onOpen = null,
            )
        }
        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_MUTED).assertCountEquals(0)
        composeTestRule.onAllNodesWithTag(Ids.NOTIFICATION_ITEM).assertCountEquals(1)
    }
}
