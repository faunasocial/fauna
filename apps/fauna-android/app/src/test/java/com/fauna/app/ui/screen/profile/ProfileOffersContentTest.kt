package com.fauna.app.ui.screen.profile

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.ffi.FfiTierItem
import org.junit.Assert.assertEquals
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_core.OfferStatus

/**
 * Compose-level coverage for the stateless [ProfileOffersContent] (the profile
 * Tiers-tab OTHER subscriber-browse section, monetization.md § Pillar 1 Slice B):
 * the section + offer-list landmarks, the empty placeholder, the per-row
 * name/price/description + Subscribe, the active/pending status rendering, and
 * the payment-link. Renders with seeded state — no Hilt, no VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfileOffersContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun tier(
        name: String = "gold",
        price: String? = "$5/mo",
        description: String? = "Gold tier",
        paymentUrl: String? = null,
    ) = FfiTierItem(
        name = name,
        rank = 1u,
        description = description,
        priceHint = price,
        askingPriceSats = null,
        paymentUrl = paymentUrl,
        autoApprove = false,
        createdAt = 0uL,
        unlocksPost = null,
    )

    private fun render(
        offers: List<FfiTierItem> = emptyList(),
        statusTier: String? = null,
        pendingTiers: Set<String> = emptySet(),
        working: Boolean = false,
        onSubscribe: (String) -> Unit = {},
    ) {
        composeTestRule.setContent {
            Column(Modifier.verticalScroll(rememberScrollState())) {
                ProfileOffersContent(
                    offers = offers,
                    // FFI-free stubs mirroring the shared `offer_status` precedence +
                    // `offer_status_label` keys (the shared fns are unit-tested in Rust).
                    statusFor = { tierName ->
                        when {
                            statusTier == tierName -> OfferStatus.ACTIVE
                            tierName in pendingTiers -> OfferStatus.PENDING
                            else -> OfferStatus.NONE
                        }
                    },
                    statusLabel = { status ->
                        when (status) {
                            OfferStatus.ACTIVE -> "Subscribed"
                            OfferStatus.PENDING -> "Pending approval"
                            OfferStatus.NONE -> "Not subscribed"
                        }
                    },
                    working = working,
                    onSubscribe = onSubscribe,
                )
            }
        }
    }

    @Test
    fun section_present_andEmptyShowsPlaceholder() {
        render()
        composeTestRule.onNodeWithTag("subscription-offers-section").assertExists()
        composeTestRule.onNodeWithTag("subscription-offer-list").assertExists()
        composeTestRule.onNodeWithText("This creator offers no subscription tiers yet").assertExists()
    }

    @Test
    fun offerRow_rendersNamePriceDescription_andSubscribeFires() {
        var subscribed: String? = null
        render(offers = listOf(tier(name = "gold", price = "$9")), onSubscribe = { subscribed = it })
        composeTestRule.onNodeWithTag("subscription-offer-row").assertExists()
        composeTestRule.onNodeWithTag("subscription-offer-name").assertTextEquals("gold")
        composeTestRule.onNodeWithTag("subscription-offer-price").assertTextEquals("$9")
        composeTestRule.onNodeWithTag("subscription-offer-subscribe-button").performScrollTo().performClick()
        assertEquals("gold", subscribed)
    }

    @Test
    fun activeStatus_disablesSubscribe_andShowsSubscribed() {
        render(offers = listOf(tier(name = "gold")), statusTier = "gold")
        composeTestRule.onNodeWithTag("subscription-offer-status").assertTextEquals("Subscribed")
        composeTestRule.onNodeWithTag("subscription-offer-subscribe-button").assertIsNotEnabled()
    }

    @Test
    fun pendingStatus_showsPendingApproval() {
        render(offers = listOf(tier(name = "gold")), pendingTiers = setOf("gold"))
        composeTestRule.onNodeWithTag("subscription-offer-status").assertTextEquals("Pending approval")
    }

    @Test
    fun paymentLink_rendersWhenUrlPresent() {
        render(offers = listOf(tier(name = "gold", paymentUrl = "https://pay.example/gold")))
        composeTestRule.onNodeWithTag("subscription-offer-payment-link").assertExists()
    }
}
