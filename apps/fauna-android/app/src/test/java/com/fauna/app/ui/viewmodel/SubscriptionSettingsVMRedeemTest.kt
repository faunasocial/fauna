package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.payments.ClaimRedeemResult
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock

/**
 * The `subscription-settings` redeem door goes through the same cross-nest
 * method as the profile Tiers tab's, with NO home and no creator: the page
 * knows neither, so the shared method redeems on this nest and never reads
 * the author slot (`monetization.md` § Pillar 3 → *The doors*).
 */
@ExperimentalCoroutinesApi
class SubscriptionSettingsVMRedeemTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    @Test
    fun `the settings redeem names no home and no creator`() {
        val vm = SubscriptionSettingsVM(mock(ApiClient::class.java))
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
        val vm = SubscriptionSettingsVM(mock(ApiClient::class.java))
        vm.redeem = { _, _, _ -> error("must not be called") }
        vm.redeemClaim("   ")
        assertEquals(false, vm.claimRedeemed.value)
    }
}
