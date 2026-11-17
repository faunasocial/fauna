package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.ApiClient
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiFeatureRow
import com.fauna.app.payments.ClaimRedeemResult
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.robolectric.annotation.Config
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.Possibility
import org.mockito.Mockito.mock
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * The OTHER profile Tiers tab's claim redeem (`monetization.md` § Pillar 3 →
 * *Which nest an in-app redeem binds on* → *The doors*; lifts tui's
 * `the_redeem_goes_to_the_viewed_creators_home` /
 * `a_redeemed_code_clears_the_input_and_marks_the_tier_pending`): the code
 * redeems under the VIEWED creator's id at the page's actor home, a blank
 * buffer sends nothing, a taken code leaves the input and marks its tier
 * pending, a refusal keeps the code. The redeem is the VM's seam, the re-read a
 * mocked [ApiClient]. The button waits on the `payments` features read THIS
 * page issues at its load (`docs/goal/ui/README.md` § Offer only what is
 * possible, rule 5), and the redeem asks that answer again (rule 6) — the live
 * shared gate over UniFFI (`just android-host-test`).
 */
@ExperimentalCoroutinesApi
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34])
class ProfileOffersVMRedeemTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val author = "ab".repeat(32)
    private val home = "https://author-home.test"

    private data class Call(val code: String, val author: String?, val home: String?)

    private class Harness(val vm: ProfileOffersVM, val api: ApiClient, val calls: MutableList<Call>)

    private val context: Context = ApplicationProvider.getApplicationContext()

    private fun harness(
        features: suspend () -> List<FfiFeatureRow> = { listOf(featureRow("payments", "available")) },
        reply: () -> ClaimRedeemResult,
    ): Harness {
        val api = mock(ApiClient::class.java)
        // An unstubbed suspend mock answers null, which the offers filter
        // would trip on and surface as an error.
        runBlocking { whenever(api.subscriptionOffersList(author, null)).thenReturn(emptyList()) }
        val calls = mutableListOf<Call>()
        val vm = ProfileOffersVM(api, context)
        vm.readFeatures = features
        vm.redeem = { code, a, h ->
            calls += Call(code, a, h)
            reply()
        }
        vm.load(author, null)
        return Harness(vm, api, calls)
    }

    @Test
    fun `the redeem goes to the viewed creator's home with the trimmed code`() {
        val h = harness { ClaimRedeemResult(tier = "gold", queued = false) }
        h.vm.setRedeemInput("  CODE42 ")
        assertEquals("  CODE42 ", h.vm.redeemInput.value)
        h.vm.redeemClaim(author, home)
        assertEquals(listOf(Call("CODE42", author, home)), h.calls)
    }

    @Test
    fun `a blank code is a no-op, never a nest round-trip`() {
        val h = harness { error("must not be called") }
        h.vm.redeemClaim(author, home)
        h.vm.setRedeemInput("   ")
        h.vm.redeemClaim(author, home)
        assertTrue(h.calls.isEmpty())
        assertNull(h.vm.errorMessage.value)
    }

    @Test
    fun `a taken code clears the input, marks a queued tier pending and re-reads`() {
        val h = harness { ClaimRedeemResult(tier = "gold", queued = true) }
        h.vm.setRedeemInput("CODE42")
        h.vm.redeemClaim(author, null)
        assertEquals("", h.vm.redeemInput.value)
        assertEquals(setOf("gold"), h.vm.pendingTiers.value)
        assertNull(h.vm.errorMessage.value)
        // The mount read plus the post-redeem re-read.
        runBlocking { verify(h.api, times(2)).subscriptionOffersList(author, null) }
        assertEquals(listOf(Call("CODE42", author, null)), h.calls)
    }

    @Test
    fun `a refusal keeps the code and surfaces the error`() {
        val h = harness { throw RuntimeException("This claim code was not found") }
        h.vm.setRedeemInput("CODE42")
        h.vm.redeemClaim(author, home)
        assertEquals("CODE42", h.vm.redeemInput.value)
        assertEquals("This claim code was not found", h.vm.errorMessage.value)
        assertTrue(h.vm.pendingTiers.value.isEmpty())
    }

    @Test
    fun `the profile open issues the features read the redeem waits on`() {
        var reads = 0
        val h = harness(features = { reads++; listOf(featureRow("payments", "available")) }) {
            ClaimRedeemResult(tier = "gold", queued = false)
        }
        assertEquals("the open read the plane itself, no Settings visit needed", 1, reads)
        assertEquals(Possibility.Possible, h.vm.redeemAnswer.value)
    }

    @Test
    fun `a failed features read blocks the redeem and the press refuses with its reason`() {
        val h = harness(features = { throw RuntimeException("offline") }) { error("must not be called") }
        assertEquals(Possibility.Blocked(LocalizedText("common.load_failed", emptyMap())), h.vm.redeemAnswer.value)
        h.vm.setRedeemInput("CODE42")
        h.vm.redeemClaim(author, home)
        assertTrue(h.calls.isEmpty())
        assertEquals("Failed to load", h.vm.errorMessage.value)
        assertEquals("CODE42", h.vm.redeemInput.value)
    }

    @Test
    fun `a nest without the payments plane leaves the redeem absent`() {
        val h = harness(features = { emptyList() }) { error("must not be called") }
        assertEquals(Possibility.Absent, h.vm.redeemAnswer.value)
        h.vm.setRedeemInput("CODE42")
        h.vm.redeemClaim(author, home)
        assertTrue(h.calls.isEmpty())
    }

    @Test
    fun `a fresh open of another creator drops the typed code`() {
        val h = harness { ClaimRedeemResult(tier = "gold", queued = false) }
        h.vm.setRedeemInput("CODE42")
        h.vm.load(author, null)
        assertEquals("CODE42", h.vm.redeemInput.value)
        h.vm.load("cd".repeat(32), null)
        assertEquals("", h.vm.redeemInput.value)
    }
}
