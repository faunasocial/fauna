package com.fauna.app.ui.screen.settings

import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.core.HexUtil
import com.fauna.ffi.FfiMineSubscription
import org.junit.Assert.assertArrayEquals
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config

/**
 * Compose-level coverage for the stateless [SubscriptionSettingsContent] (the
 * consumer-side `subscription-settings` page, monetization.md § Pillar 1 Slice
 * B): the page heading + section landmark, the empty placeholder, the indexed
 * `subscription-mine-row` rows (author = handle or hex fallback, tier, status),
 * and the per-row unsubscribe. Renders with seeded state — no Hilt, no VM, no
 * FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class SubscriptionSettingsContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun sub(
        authorId: ByteArray = ByteArray(32) { 5 },
        tier: String = "gold",
        status: String = "active",
        handle: String? = "alice",
        // Mirrors what `fauna_core::format::author_display_label` pre-computes
        // for this fixture (handle if non-blank, else the full hex actor id) —
        // the screen renders `sub.authorDisplay` verbatim, no local fallback.
        authorDisplay: String = handle ?: HexUtil.bytesToHex(authorId),
        homeNestUrl: String? = null,
    ) = FfiMineSubscription(
        authorId = authorId,
        tier = tier,
        status = status,
        handle = handle,
        since = 0uL,
        authorDisplay = authorDisplay,
        homeNestUrl = homeNestUrl,
    )

    private fun render(
        subscriptions: List<FfiMineSubscription> = emptyList(),
        working: Boolean = false,
        onUnsubscribe: (FfiMineSubscription) -> Unit = {},
        onRedeemClaim: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            SubscriptionSettingsContent(
                subscriptions = subscriptions,
                working = working,
                onBack = {},
                onUnsubscribe = onUnsubscribe,
                onRedeemClaim = onRedeemClaim,
            )
        }
    }

    @Test
    fun claimRedeem_inputAndButton_fireWithTypedCode() {
        var redeemed: String? = null
        render(onRedeemClaim = { redeemed = it })
        composeTestRule.onNodeWithTag("subscription-claim-redeem-input")
            .performScrollTo().performTextInput("ABCD-1234")
        composeTestRule.onNodeWithTag("subscription-claim-redeem-button")
            .performScrollTo().performClick()
        assertEquals("ABCD-1234", redeemed)
    }

    @Test
    fun emptyState_showsHeadingSectionAndPlaceholder() {
        render()
        composeTestRule.onNodeWithTag("page-heading").assertExists()
        composeTestRule.onNodeWithTag("subscription-mine-section").assertExists()
        composeTestRule.onNodeWithText("You have no subscriptions yet").assertIsDisplayed()
    }

    /** `subscriptions.md` § Consumer side: before the `mine.list` read answers
     *  the list says loading, never "no subscriptions"; a failed first read
     *  paints neither (its reason is on `error-message`). */
    @Test
    fun theMineListIsThreeState() {
        var read by mutableStateOf(uniffi.fauna_core.ReadState.PENDING)
        composeTestRule.setContent {
            SubscriptionSettingsContent(
                subscriptions = emptyList(),
                working = false,
                onBack = {},
                onUnsubscribe = {},
                mineRead = read,
            )
        }
        composeTestRule.onNodeWithText("You have no subscriptions yet").assertDoesNotExist()
        composeTestRule.onNodeWithText("Loading...").assertExists()
        read = uniffi.fauna_core.ReadState.FAILED
        composeTestRule.onNodeWithText("You have no subscriptions yet").assertDoesNotExist()
        composeTestRule.onNodeWithText("Loading...").assertDoesNotExist()
        read = uniffi.fauna_core.ReadState.RESOLVED
        composeTestRule.onNodeWithText("You have no subscriptions yet").assertExists()
    }

    @Test
    fun mineRow_rendersAuthorTierStatus() {
        render(subscriptions = listOf(sub(tier = "gold", status = "pending", handle = "alice")))
        composeTestRule.onNodeWithTag("subscription-mine-row").assertExists()
        composeTestRule.onNodeWithTag("subscription-mine-author").assertTextEquals("alice")
        composeTestRule.onNodeWithTag("subscription-mine-tier").assertTextEquals("gold")
        composeTestRule.onNodeWithTag("subscription-mine-status").assertTextEquals("pending")
    }

    @Test
    fun mineRow_handleFallsBackToHexActorId() {
        val bytes = ByteArray(32) { 7 }
        render(subscriptions = listOf(sub(authorId = bytes, handle = null)))
        composeTestRule.onNodeWithTag("subscription-mine-author")
            .assertTextEquals(HexUtil.bytesToHex(bytes))
    }

    @Test
    fun unsubscribe_firesWithTheRowCarryingTierAndHome() {
        val bytes = ByteArray(32) { 9 }
        var unsubscribed: FfiMineSubscription? = null
        render(
            subscriptions = listOf(sub(authorId = bytes, homeNestUrl = "https://home.example")),
            onUnsubscribe = { unsubscribed = it },
        )
        composeTestRule.onNodeWithTag("subscription-mine-unsubscribe-button").performClick()
        assertTrue(unsubscribed != null)
        assertArrayEquals(bytes, unsubscribed!!.authorId)
        assertEquals("gold", unsubscribed!!.tier)
        assertEquals("https://home.example", unsubscribed!!.homeNestUrl)
    }
}
