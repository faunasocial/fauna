package com.fauna.app.ui.screen.profile

import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.ui.Modifier
import androidx.compose.ui.test.*
import androidx.compose.ui.test.junit4.createComposeRule
import com.fauna.app.core.HexUtil
import com.fauna.app.payments.ClaimItem
import com.fauna.app.payments.ProviderItem
import com.fauna.ffi.FfiPendingRequest
import com.fauna.ffi.FfiSubscriberEntry
import com.fauna.ffi.FfiTierItem
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.runner.RunWith
import com.fauna.app.testing.FaunaRobolectricTestRunner
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText

/**
 * Compose-level coverage for the stateless [ProfileTiersContent] (the profile
 * Tiers-tab SELF author-management sections, monetization.md § Pillar 1 Slice A
 * + § Pillar 3 Slices — the webhook-URL preview, paid badge, and §5 manual
 * claims added 2026-07-16): the section landmarks, the create-tier form reveal
 * + save, the indexed tier / pending-request / subscriber / provider / claim
 * rows with their per-row controls, the `subscription-request-busy` indicator,
 * and the payment-verified paid badge. Renders with seeded state — no Hilt, no
 * VM, no FFI native calls.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfileTiersContentTest {

    @get:Rule
    val composeTestRule = createComposeRule()

    private fun tier(
        name: String = "gold",
        rank: UInt = 1u,
        price: String? = "$5/mo",
        autoApprove: Boolean = false,
        unlocksPost: String? = null,
        askingPriceSats: ULong? = null,
    ) = FfiTierItem(
        name = name,
        rank = rank,
        description = null,
        priceHint = price,
        askingPriceSats = askingPriceSats,
        paymentUrl = null,
        autoApprove = autoApprove,
        createdAt = 0uL,
        unlocksPost = unlocksPost,
    )

    private fun request(
        id: Long = 1L,
        kind: String = "subscribe",
        tier: String = "gold",
        paymentEntitled: Boolean = false,
    ) = FfiPendingRequest(
        requestId = id,
        subscriberId = ByteArray(32) { 7 },
        tierName = tier,
        kind = kind,
        createdAt = 0uL,
        mlkemEncapsKey = null,
        paymentEntitled = paymentEntitled,
    )

    private fun subscriber() = FfiSubscriberEntry(
        subscriberId = ByteArray(32) { 9 },
        joinedAt = 0uL,
        mlkemEncapsKey = null,
    )

    private fun claim(
        code: String = "MANUAL-1",
        tier: String = "gold",
        redeemedBy: ByteArray? = null,
        voidedAt: ULong? = null,
    ) = ClaimItem(
        code = code,
        tier = tier,
        provider = "manual",
        validUntil = null,
        createdAt = 0uL,
        redeemedBy = redeemedBy,
        redeemedAt = if (redeemedBy != null) 0uL else null,
        voidedAt = voidedAt,
    )

    /** FFI-free stub matching `claim_status_label`'s 3-state shape (redeemed
     *  wins over voided) — the key alone is asserted (no generated string
     *  resource exists in this harness, so [resolveLocalized] falls back to
     *  returning the raw key, exactly like a missing-translation key would). */
    private fun testClaimStatusLabel(redeemed: Boolean, voided: Boolean): LocalizedText =
        LocalizedText(
            key = when {
                redeemed -> "test.claim_status_redeemed"
                voided -> "test.claim_status_voided"
                else -> "test.claim_status_unredeemed"
            },
            args = emptyMap(),
        )

    /** FFI-free stub matching `provider_status_label`'s evidence-based 3-state
     *  shape (both null → configured; most-recent evidence wins, a tie resolves
     *  to error) — the key alone is asserted, mirroring [testClaimStatusLabel]. */
    private fun testProviderStatusLabel(lastVerifiedAt: ULong?, lastRejectedAt: ULong?): LocalizedText =
        LocalizedText(
            key = when {
                lastRejectedAt == null ->
                    if (lastVerifiedAt != null) "test.provider_status_verified"
                    else "test.provider_status_configured"
                lastVerifiedAt != null && lastVerifiedAt > lastRejectedAt -> "test.provider_status_verified"
                else -> "test.provider_status_error"
            },
            args = emptyMap(),
        )

    private fun render(
        tiers: List<FfiTierItem> = emptyList(),
        requests: List<FfiPendingRequest> = emptyList(),
        subscribers: List<FfiSubscriberEntry> = emptyList(),
        selectedTier: Int = 0,
        approving: Boolean = false,
        working: Boolean = false,
        onCreate: (String, UInt, String?, String?, String?, Boolean, ULong?) -> Unit = { _, _, _, _, _, _, _ -> },
        onUpdate: (String, UInt?, String?, String?, String?, Boolean?, ULong?) -> Unit = { _, _, _, _, _, _, _ -> },
        onDelete: (String) -> Unit = {},
        onApprove: (FfiPendingRequest) -> Unit = {},
        onReject: (Long) -> Unit = {},
        onSelectTier: (Int) -> Unit = {},
        onRemove: (String, ByteArray) -> Unit = { _, _ -> },
        providers: List<ProviderItem> = emptyList(),
        providerKinds: List<String> = emptyList(),
        claims: List<ClaimItem> = emptyList(),
        onSetProvider: (String, String, String) -> Unit = { _, _, _ -> },
        onRemoveProvider: (String) -> Unit = {},
        onMintClaim: (String) -> Unit = {},
        webhookUrl: (String) -> String = { kind -> "https://example.test/webhook/$kind" },
        // FFI-free stand-in for `com.fauna.ffi.parseCount`, mirroring
        // `fauna_core::format::parse_count` exactly (`input.trim().parse::<u32>().ok()`)
        // — the TRIM is the whole point of the swap this stub covers.
        parseRank: (String) -> UInt? = { it.trim().toUIntOrNull() },
    ) {
        composeTestRule.setContent {
            // Mirror ProfileScreen's verticalScroll so off-screen controls (the
            // tall create form, the bottom subscriber roster) are scroll-reachable.
            Column(Modifier.verticalScroll(rememberScrollState())) {
            ProfileTiersContent(
                tiers = tiers,
                requests = requests,
                subscribers = subscribers,
                selectedTier = selectedTier,
                approving = approving,
                working = working,
                onCreate = onCreate,
                onUpdate = onUpdate,
                onDelete = onDelete,
                onApprove = onApprove,
                onReject = onReject,
                onSelectTier = onSelectTier,
                onRemove = onRemove,
                providers = providers,
                providerKinds = providerKinds,
                claims = claims,
                onSetProvider = onSetProvider,
                onRemoveProvider = onRemoveProvider,
                onMintClaim = onMintClaim,
                // FFI-free stub matching `hex_full` output — keeps the harness
                // off the native path (the real screen injects `com.fauna.ffi.hexFull`).
                hexFull = { HexUtil.bytesToHex(it) },
                webhookUrl = webhookUrl,
                claimStatusLabel = ::testClaimStatusLabel,
                providerStatusLabel = ::testProviderStatusLabel,
                parseRank = parseRank,
            )
            }
        }
    }

    @Test
    fun threeSections_alwaysPresent_evenWhenEmpty() {
        render()
        composeTestRule.onNodeWithTag("subscription-tiers-section").assertExists()
        composeTestRule.onNodeWithTag("subscription-requests-section").assertExists()
        composeTestRule.onNodeWithTag("subscription-subscribers-section").assertExists()
        composeTestRule.onNodeWithTag("subscription-tier-create-button").assertIsDisplayed()
        composeTestRule.onNodeWithTag("subscription-subscribers-tier-select").assertExists()
        composeTestRule.onNodeWithTag("subscription-provider-section").assertExists()
        composeTestRule.onNodeWithTag("subscription-provider-add-button").assertExists()
    }

    private fun provider(
        kind: String = "fake",
        tier: String = "gold",
        lastVerifiedAt: ULong? = null,
        lastRejectedAt: ULong? = null,
    ) = ProviderItem(
        kind = kind,
        tier = tier,
        createdAt = 0uL,
        lastVerifiedAt = lastVerifiedAt,
        lastRejectedAt = lastRejectedAt,
    )

    @Test
    fun providerRow_rendersKindStatus_andRemoveFires() {
        var removed: String? = null
        render(
            providers = listOf(provider(kind = "fake", tier = "gold")),
            onRemoveProvider = { removed = it },
        )
        composeTestRule.onNodeWithTag("subscription-provider-row").assertExists()
        composeTestRule.onNodeWithTag("subscription-provider-kind").assertTextEquals("fake")
        // Evidence-based status (provider_status_label): a fresh row with no
        // verified/rejected stamps renders "configured" — see ProviderRow.
        composeTestRule.onNodeWithTag("subscription-provider-status")
            .assertTextEquals("test.provider_status_configured")
        composeTestRule.onNodeWithTag("subscription-provider-remove-button")
            .performScrollTo().performClick()
        assertEquals("fake", removed)
    }

    @Test
    fun providerRow_rendersVerified_whenVerifiedWithNoRejection() {
        render(providers = listOf(provider(lastVerifiedAt = 100uL, lastRejectedAt = null)))
        composeTestRule.onNodeWithTag("subscription-provider-status")
            .assertTextEquals("test.provider_status_verified")
    }

    @Test
    fun providerRow_rendersError_whenRejectionIsMostRecent() {
        render(providers = listOf(provider(lastVerifiedAt = 100uL, lastRejectedAt = 200uL)))
        composeTestRule.onNodeWithTag("subscription-provider-status")
            .assertTextEquals("test.provider_status_error")
    }

    @Test
    fun providerRow_rendersVerified_whenVerificationIsMostRecent() {
        render(providers = listOf(provider(lastVerifiedAt = 200uL, lastRejectedAt = 100uL)))
        composeTestRule.onNodeWithTag("subscription-provider-status")
            .assertTextEquals("test.provider_status_verified")
    }

    @Test
    fun providerForm_opens_neverPrefillsSecret_andSaveFires() {
        var saved: Triple<String, String, String>? = null
        render(
            tiers = listOf(tier(name = "gold")),
            providerKinds = listOf("fake", "stripe"),
            onSetProvider = { kind, secret, tier -> saved = Triple(kind, secret, tier) },
        )
        composeTestRule.onNodeWithTag("subscription-provider-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("subscription-provider-form").assertExists()
        composeTestRule.onNodeWithTag("subscription-provider-form-secret")
            .performScrollTo().performTextInput("whsec_test")
        composeTestRule.onNodeWithTag("subscription-provider-form-save")
            .performScrollTo().performClick()
        // Kind defaults to the registry's first entry; tier to the author's first tier.
        assertEquals(Triple("fake", "whsec_test", "gold"), saved)
    }

    @Test
    fun providerForm_saveDisabled_withNoTiers() {
        render(providerKinds = listOf("fake"))
        composeTestRule.onNodeWithTag("subscription-provider-add-button")
            .performScrollTo().performClick()
        composeTestRule.onNodeWithTag("subscription-provider-form-save")
            .performScrollTo().assertIsNotEnabled()
    }

    @Test
    fun tierRow_rendersNameRankPrice() {
        render(tiers = listOf(tier(name = "gold", rank = 2u, price = "$9")))
        composeTestRule.onNodeWithTag("subscription-tier-row").assertExists()
        composeTestRule.onNodeWithTag("subscription-tier-name").assertTextEquals("gold")
        composeTestRule.onNodeWithTag("subscription-tier-rank").assertTextEquals("2")
        composeTestRule.onNodeWithTag("subscription-tier-price").assertTextEquals("$9")
    }

    @Test
    fun tierRow_excludesDesignatedUnlockTiers() {
        // leg (2), monetization.md:128 — an auto-minted "sell this post" tier
        // (carrying `unlocksPost`) is not one the author manages, so §1 excludes
        // it; §3/§4/§5 still need to pick it, so this test only asserts §1.
        render(
            tiers = listOf(
                tier(name = "gold"),
                tier(name = "post-unlock-deadbeef", unlocksPost = "deadbeefdeadbeef"),
            ),
        )
        composeTestRule.onAllNodesWithTag("subscription-tier-row").assertCountEquals(1)
        composeTestRule.onNodeWithTag("subscription-tier-name").assertTextEquals("gold")
    }

    @Test
    fun createButton_opensForm_andSaveFiresOnCreate() {
        var created: String? = null
        render(onCreate = { name, _, _, _, _, _, _ -> created = name })
        composeTestRule.onNodeWithTag("subscription-tier-create-button").performClick()
        composeTestRule.onNodeWithTag("subscription-tier-form").assertExists()
        composeTestRule.onNodeWithTag("subscription-tier-form-name")
            .performScrollTo().performTextInput("silver")
        composeTestRule.onNodeWithTag("subscription-tier-form-save")
            .performScrollTo().performClick()
        assertEquals("silver", created)
    }

    /**
     * The rank field's value reaches the submit through the INJECTED parser —
     * i.e. through `com.fauna.ffi.parseCount` in production, never Kotlin's own
     * `toUIntOrNull` (`value-formatting.md` § Tier rank: the § Mail-knob
     * validation parser reused, no new fn).
     *
     * **Why the assertion is a sentinel and not a value difference.** The two
     * parsers disagree only on surrounding whitespace — the shared one trims —
     * and this field's `onValueChange` strips every non-digit as it is typed, so
     * no input the user can produce distinguishes them. A test phrased as "type
     * ' 4 ', expect 4" would therefore pass just as well against the old
     * `toUIntOrNull()` call: vacuous coverage that reads like proof. Injecting a
     * parser that answers a value neither the field nor the fallback can produce
     * is the honest discriminator — the old code ignores the injection entirely
     * and yields the typed `7u`, so reverting the call site reds this.
     *
     * What it does NOT claim: that the swap changed any behaviour. It did not,
     * and the digit filter is why (see the `parseRank` comment on the composable).
     * This pins the wiring, which is the whole content of android's leg.
     */
    @Test
    fun tierForm_routesTheRankThroughTheInjectedSharedParser() {
        var savedRank: UInt? = null
        render(
            onCreate = { _, rank, _, _, _, _, _ -> savedRank = rank },
            parseRank = { 4242u },
        )
        composeTestRule.onNodeWithTag("subscription-tier-create-button").performClick()
        composeTestRule.onNodeWithTag("subscription-tier-form-name")
            .performScrollTo().performTextInput("silver")
        composeTestRule.onNodeWithTag("subscription-tier-form-rank")
            .performScrollTo().performTextInput("7")
        composeTestRule.onNodeWithTag("subscription-tier-form-save")
            .performScrollTo().performClick()
        assertEquals(4242u, savedRank)
    }

    /**
     * The `?: 0u` fallback survives the swap: a parser that refuses the input
     * (the shared fn returns `None` for empty — the state an untouched rank
     * field is in) still submits 0, unchanged from before.
     */
    @Test
    fun tierForm_keepsTheZeroFallback_whenTheSharedParserRefuses() {
        var savedRank: UInt? = null
        render(
            onCreate = { _, rank, _, _, _, _, _ -> savedRank = rank },
            parseRank = { null },
        )
        composeTestRule.onNodeWithTag("subscription-tier-create-button").performClick()
        composeTestRule.onNodeWithTag("subscription-tier-form-name")
            .performScrollTo().performTextInput("silver")
        composeTestRule.onNodeWithTag("subscription-tier-form-save")
            .performScrollTo().performClick()
        assertEquals(0u, savedRank)
    }

    // ── The machine-comparable asking price (monetization.md § The asking ──
    // price) — independent of price_hint; no parsing ever infers one from
    // the other.

    @Test
    fun tierForm_rendersAskingPriceField() {
        render()
        composeTestRule.onNodeWithTag("subscription-tier-create-button").performClick()
        composeTestRule.onNodeWithTag("subscription-tier-form-asking-price")
            .performScrollTo().assertExists()
    }

    @Test
    fun tierForm_savePassesTheTypedAskingPriceAsSats() {
        var savedAskingPrice: ULong? = null
        render(onCreate = { _, _, _, _, _, _, askingPrice -> savedAskingPrice = askingPrice })
        composeTestRule.onNodeWithTag("subscription-tier-create-button").performClick()
        composeTestRule.onNodeWithTag("subscription-tier-form-name")
            .performScrollTo().performTextInput("silver")
        composeTestRule.onNodeWithTag("subscription-tier-form-asking-price")
            .performScrollTo().performTextInput("500")
        composeTestRule.onNodeWithTag("subscription-tier-form-save")
            .performScrollTo().performClick()
        assertEquals(500uL, savedAskingPrice)
    }

    @Test
    fun tierForm_emptyAskingPriceSavesNull_neverClearingBySendingZero() {
        // Empty on create means unpriced; empty on an edit means "keep the
        // current price" (`tiers.update`'s merge rule) — either way, `null`,
        // never a sent `0` that would silently price the tier at zero.
        var savedAskingPrice: ULong? = 1uL
        render(onCreate = { _, _, _, _, _, _, askingPrice -> savedAskingPrice = askingPrice })
        composeTestRule.onNodeWithTag("subscription-tier-create-button").performClick()
        composeTestRule.onNodeWithTag("subscription-tier-form-name")
            .performScrollTo().performTextInput("silver")
        composeTestRule.onNodeWithTag("subscription-tier-form-save")
            .performScrollTo().performClick()
        assertEquals(null, savedAskingPrice)
    }

    @Test
    fun pendingRequest_rendersKind_andApproveFires() {
        var approved = false
        render(requests = listOf(request(kind = "subscribe")), onApprove = { approved = true })
        composeTestRule.onNodeWithTag("subscription-request-row").assertExists()
        composeTestRule.onNodeWithTag("subscription-request-kind").assertTextEquals("subscribe")
        composeTestRule.onNodeWithTag("subscription-request-approve-button").performClick()
        assertTrue(approved)
    }

    @Test
    fun approving_showsBusyIndicator() {
        render(requests = listOf(request()), approving = true)
        composeTestRule.onNodeWithTag("subscription-request-busy").assertIsDisplayed()
    }

    @Test
    fun paidBadge_showsOnlyWhenPaymentEntitled() {
        render(requests = listOf(request(id = 1L, paymentEntitled = true)))
        composeTestRule.onNodeWithTag("subscription-request-paid-badge").assertExists()
    }

    @Test
    fun paidBadge_absentWhenNotPaymentEntitled() {
        render(requests = listOf(request(id = 1L, paymentEntitled = false)))
        composeTestRule.onNodeWithTag("subscription-request-paid-badge").assertDoesNotExist()
    }

    @Test
    fun webhookUrlPreview_recomputesFromKind_andCopyButtonCopies() {
        render(
            tiers = listOf(tier(name = "gold")),
            providerKinds = listOf("fake", "stripe"),
            webhookUrl = { kind -> "https://example.test/webhook/$kind" },
        )
        composeTestRule.onNodeWithTag("subscription-provider-add-button")
            .performScrollTo().performClick()
        // Kind select defaults to the registry's first entry ("fake").
        composeTestRule.onNodeWithTag("subscription-provider-form-webhook-url")
            .performScrollTo()
            .assert(hasText("https://example.test/webhook/fake"))
        composeTestRule.onNodeWithTag("subscription-provider-form-webhook-url-copy-button")
            .assertExists()
    }

    @Test
    fun subscriberRow_removeFiresWithSelectedTier() {
        var removedTier: String? = null
        render(
            tiers = listOf(tier(name = "gold")),
            subscribers = listOf(subscriber()),
            selectedTier = 0,
            onRemove = { tierName, _ -> removedTier = tierName },
        )
        composeTestRule.onNodeWithTag("subscription-subscriber-row").assertExists()
        composeTestRule.onNodeWithTag("subscription-subscriber-remove-button")
            .performScrollTo().performClick()
        assertEquals("gold", removedTier)
    }

    @Test
    fun claimSection_alwaysPresent_evenWhenEmpty() {
        render()
        composeTestRule.onNodeWithTag("subscription-claim-section").assertExists()
        composeTestRule.onNodeWithTag("subscription-claim-tier-select").assertExists()
        composeTestRule.onNodeWithTag("subscription-claim-mint-button").assertExists()
    }

    @Test
    fun claimMint_firesWithSelectedTier() {
        var minted: String? = null
        render(tiers = listOf(tier(name = "gold")), onMintClaim = { minted = it })
        composeTestRule.onNodeWithTag("subscription-claim-mint-button")
            .performScrollTo().performClick()
        // Tier select defaults to the author's first tier.
        assertEquals("gold", minted)
    }

    @Test
    fun claimRow_rendersCodeTierStatus_redeemedWinsOverVoided() {
        render(
            claims = listOf(
                claim(
                    code = "MANUAL-1",
                    tier = "gold",
                    redeemedBy = ByteArray(32) { 3 },
                    voidedAt = 1uL,
                ),
            ),
        )
        composeTestRule.onNodeWithTag("subscription-claim-row").assertExists()
        composeTestRule.onNodeWithTag("subscription-claim-code").assertTextEquals("MANUAL-1")
        composeTestRule.onNodeWithTag("subscription-claim-tier").assertTextEquals("gold")
        // Both redeemed_by and voided_at set — redeemed wins (the shared
        // fauna_core::format::claim_status_label decision).
        composeTestRule.onNodeWithTag("subscription-claim-status")
            .assertTextEquals("test.claim_status_redeemed")
    }

    @Test
    fun claimRow_unredeemedStatus_whenNeitherRedeemedNorVoided() {
        render(claims = listOf(claim(redeemedBy = null, voidedAt = null)))
        composeTestRule.onNodeWithTag("subscription-claim-status")
            .assertTextEquals("test.claim_status_unredeemed")
    }
}
