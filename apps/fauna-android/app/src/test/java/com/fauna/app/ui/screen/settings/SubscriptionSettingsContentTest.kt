package com.fauna.app.ui.screen.settings

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
    ) = FfiMineSubscription(
        authorId = authorId,
        tier = tier,
        status = status,
        handle = handle,
        since = 0uL,
        authorDisplay = authorDisplay,
    )

    private fun render(
        subscriptions: List<FfiMineSubscription> = emptyList(),
        working: Boolean = false,
        onUnsubscribe: (ByteArray) -> Unit = {},
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
    fun unsubscribe_firesWithAuthorId() {
        val bytes = ByteArray(32) { 9 }
        var unsubscribed: ByteArray? = null
        render(
            subscriptions = listOf(sub(authorId = bytes)),
            onUnsubscribe = { unsubscribed = it },
        )
        composeTestRule.onNodeWithTag("subscription-mine-unsubscribe-button").performClick()
        assertTrue(unsubscribed != null)
        assertArrayEquals(bytes, unsubscribed)
    }
}
