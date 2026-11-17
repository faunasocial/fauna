package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.ApiClient
import com.fauna.app.payments.ClaimRedeemResult
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiFeatureRow
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility
import org.mockito.Mockito.`when` as whenever

/**
 * The `subscription-settings` redeem door goes through the same cross-nest
 * method as the profile Tiers tab's, with NO home and no creator: the page
 * knows neither, so the shared method redeems on this nest and never reads
 * the author slot (`monetization.md` § Pillar 3 → *The doors*).
 *
 * The button waits on the `payments` features read (`docs/goal/ui/README.md`
 * § Offer only what is possible, rule 5): blocked still loading before it
 * answers, load-failed when it threw, absent without the plane — and the
 * redeem asks that same answer again, refusing with its reason (rule 6). The
 * answer is the live shared gate over UniFFI (`just android-host-test`).
 */
@ExperimentalCoroutinesApi
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class SubscriptionSettingsVMRedeemTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val context: Context = ApplicationProvider.getApplicationContext()

    private fun vm(features: suspend () -> List<FfiFeatureRow>): SubscriptionSettingsVM {
        val api = mock(ApiClient::class.java)
        runBlocking { whenever(api.subscriptionMineList()).thenReturn(emptyList()) }
        return SubscriptionSettingsVM(api, context).also { it.readFeatures = features }
    }

    @Test
    fun `the settings redeem names no home and no creator`() {
        val vm = vm { listOf(featureRow("payments", "available")) }
        vm.refresh()
        val calls = mutableListOf<Triple<String, String?, String?>>()
        vm.redeem = { code, author, home ->
            calls += Triple(code, author, home)
            ClaimRedeemResult(tier = "gold", queued = true)
        }
        vm.redeemClaim("  CODE42 ")
        assertEquals(listOf(Triple<String, String?, String?>("CODE42", null, null)), calls)
        assertTrue(vm.claimRedeemed.value)
    }

    @Test
    fun `a blank settings code is a no-op`() {
        val vm = vm { listOf(featureRow("payments", "available")) }
        vm.refresh()
        vm.redeem = { _, _, _ -> error("must not be called") }
        vm.redeemClaim("   ")
        assertEquals(false, vm.claimRedeemed.value)
    }

    @Test
    fun `before the features read the redeem is blocked still loading and refuses`() {
        val vm = vm { error("not issued yet") }
        assertEquals(blocked("common.still_loading"), vm.redeemAnswer.value)
        vm.redeem = { _, _, _ -> error("must not be called") }
        vm.redeemClaim("CODE42")
        assertEquals("Still loading — try again in a moment", vm.errorMessage.value)
        assertEquals(false, vm.claimRedeemed.value)
    }

    @Test
    fun `a failed features read blocks the redeem with load failed`() {
        val vm = vm { throw RuntimeException("offline") }
        vm.refresh()
        assertEquals(blocked("common.load_failed"), vm.redeemAnswer.value)
        vm.redeem = { _, _, _ -> error("must not be called") }
        vm.redeemClaim("CODE42")
        assertEquals("Failed to load", vm.errorMessage.value)
    }

    @Test
    fun `a nest without the payments plane leaves the redeem absent`() {
        val vm = vm { listOf(featureRow("payments", "hidden")) }
        vm.refresh()
        assertEquals(Possibility.Absent, vm.redeemAnswer.value)
        vm.redeem = { _, _, _ -> error("must not be called") }
        vm.redeemClaim("CODE42")
        assertEquals(false, vm.claimRedeemed.value)
        assertEquals(Possibility.Absent, vm({ emptyList() }).also { it.refresh() }.redeemAnswer.value)
    }

    private fun blocked(key: String) = Possibility.Blocked(LocalizedText(key, emptyMap()))
}

/** The shared `FfiFeatureRow` shape the real `FeaturesClient::rows()`
 *  composition returns (`NostrContentTest.featureRow`'s twin). */
internal fun featureRow(feature: String, affordance: String) = FfiFeatureRow(
    feature = feature,
    name = LocalizedText("features.$feature.name", emptyMap()),
    availability = if (affordance == "available") "allow" else "deny",
    deniedBy = null,
    cells = emptyList(),
    perOperationMax = null,
    perOperationMaxTier = null,
    unit = "bytes",
    affordance = affordance,
    restriction = null,
    status = LocalizedText("features.status.$affordance", emptyMap()),
)
