package com.fauna.app.ui.screen.bridges

import android.content.Context
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.R
import com.fauna.app.core.FeedTriple
import com.fauna.app.core.SourceAskRows
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiFeedRequestState
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config

/**
 * The render half of the ward's feed-source ask inside `bridge-card`
 * (family-safety.md § Feed-source approvals; tui's `bridges.rs` arms): which
 * rows paint is decided by `sourceAskRows` (pinned in `core/WardAsksTest`);
 * this pins what each row IS — a state label vs an ask button carrying its
 * own triple. FFI-free: the rows are seeded directly. No `performScrollTo()`:
 * the section is rendered on its own, with no scroll parent (see
 * `OfflineGateTest`'s bridge-card note).
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class SourceAskSectionContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private val ctx get() = ApplicationProvider.getApplicationContext<Context>()

    @Test
    fun emptyRowsPaintNeitherElement() {
        composeTestRule.setContent { SourceAskSection(SourceAskRows()) {} }
        composeTestRule.onNodeWithTag("bridge-source-request-button").assertDoesNotExist()
        composeTestRule.onNodeWithTag("bridge-source-request-state").assertDoesNotExist()
    }

    /** Each ask button carries ITS OWN refused triple. */
    @Test
    fun eachAskButtonIssuesItsOwnTriple() {
        val follow = FeedTriple("activitypub", "follow", "npub1abc")
        val link = FeedTriple("activitypub", "link", "")
        val asked = mutableListOf<FeedTriple>()
        composeTestRule.setContent { SourceAskSection(SourceAskRows(asks = listOf(follow, link))) { asked += it } }
        val buttons = composeTestRule.onAllNodesWithTag("bridge-source-request-button")
        buttons[1].performClick()
        buttons[0].performClick()
        assertEquals(listOf(link, follow), asked)
    }

    /** ⚠ Rule (e): an approved ask is the "try again" PROMPT — a label, never
     *  an affordance that could redeem the single-use grant on its own. */
    @Test
    fun anApprovedAskIsALabelPromptNotAButton() {
        composeTestRule.setContent {
            SourceAskSection(SourceAskRows(states = listOf(FfiFeedRequestState.APPROVED, FfiFeedRequestState.PENDING))) {}
        }
        val states = composeTestRule.onAllNodesWithTag("bridge-source-request-state")
        states[0].assertTextEquals(ctx.getString(R.string.bridges_source_request_approved))
        states[1].assertTextEquals(ctx.getString(R.string.bridges_source_request_pending))
        states[0].assertHasNoClickAction()
        composeTestRule.onNodeWithTag("bridge-source-request-button").assertDoesNotExist()
    }
}
