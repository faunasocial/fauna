package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.SubscribeCheckout
import com.fauna.ffi.FfiSubscribeReply
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
import org.mockito.ArgumentMatchers.anyString
import org.mockito.ArgumentMatchers.nullable
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.`when` as whenever

/**
 * The profile Tiers-tab payment link (`subscription-offer-payment-link`;
 * `monetization.md` § Pillar 3 → *Buyer↔actor binding (Q4)*, the Q13 rules (1)
 * and (2)): the gesture SUBSCRIBES FIRST — across nests at the page's actor
 * home, the value the header follow button passes — and only then opens the
 * checkout URL the shared rule composed, never the author's raw link. Lifts
 * tui's `Action::OpenPaymentLink` / linux `offers.rs::pay_to`. The row's plain
 * Subscribe goes across nests at the same home.
 *
 * [ApiClient] is a mock, so the shared `subscriptionsSubscribeAndCheckout` is
 * stood in for by its app-facing member; the composition rule itself is pinned
 * in `libs/fauna-client-subscriptions`.
 */
@ExperimentalCoroutinesApi
class ProfileOffersVMPaymentLinkTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val author = "ab".repeat(32)
    private val remote = "https://peer.example:9443"
    private val raw = "https://buy.stripe.com/gold"
    private val composed = "https://buy.stripe.com/gold?client_reference_id=xyz"

    /** Every call made on [api], by name, in order, with its arguments. */
    private fun calls(api: ApiClient): List<Pair<String, List<Any?>>> =
        mockingDetails(api).invocations.map { it.method.name to it.arguments.toList() }

    private fun api(checkout: () -> SubscribeCheckout?): ApiClient = mock(ApiClient::class.java).also { api ->
        runBlocking {
            whenever(api.subscriptionOffersList(anyString(), nullable(String::class.java))).thenReturn(emptyList())
            whenever(api.subscriptionSubscribeAndCheckout(anyString(), anyString(), anyString(), anyString()))
                .thenAnswer { checkout() }
        }
    }

    @Test
    fun thePaymentLinkSubscribesAtTheHomeThenOpensTheComposedCheckout() {
        val opened = mutableListOf<String>()
        val api = api { SubscribeCheckout(FfiSubscribeReply.Queued(requestId = 7L), composed) }
        val vm = ProfileOffersVM(api)

        vm.payTo(author, "gold", raw, remote) { opened += it }

        val subscribes = calls(api).filter { it.first == "subscriptionSubscribeAndCheckout" }
        assertEquals(listOf(listOf<Any?>(author, "gold", raw, remote)), subscribes.map { it.second.take(4) })
        // The composed URL opens — never the raw link — and only after the subscribe answered.
        assertEquals(listOf(composed), opened)
        // The reply lands on the row as the Subscribe button's: Queued → pending.
        assertEquals(setOf("gold"), vm.pendingTiers.value)
        assertNull(vm.errorMessage.value)
    }

    @Test
    fun anApprovedReplyReReadsTheRowLikeTheSubscribeButton() {
        val opened = mutableListOf<String>()
        val api = api { SubscribeCheckout(FfiSubscribeReply.Approved(tier = "gold", expiresAt = null), composed) }
        val vm = ProfileOffersVM(api)

        vm.payTo(author, "gold", raw, remote) { opened += it }

        assertEquals(listOf(composed), opened)
        assertTrue(calls(api).any { it.first == "subscriptionOffersList" && it.second.getOrNull(1) == remote })
        assertEquals(emptySet<String>(), vm.pendingTiers.value)
    }

    @Test
    fun nothingToOpenOpensNothing() {
        val opened = mutableListOf<String>()
        val vm = ProfileOffersVM(api { null })

        vm.payTo(author, "gold", raw, remote) { opened += it }

        assertEquals(emptyList<String>(), opened)
        assertNull(vm.errorMessage.value)
    }

    @Test
    fun aRefusedSubscribeIsThePageErrorAndOpensNothing() {
        val opened = mutableListOf<String>()
        val vm = ProfileOffersVM(api { throw RuntimeException("subscribe refused") })

        vm.payTo(author, "gold", raw, remote) { opened += it }

        assertEquals(emptyList<String>(), opened)
        assertEquals("subscribe refused", vm.errorMessage.value)
        assertEquals(false, vm.working.value)
    }

    @Test
    fun theRowSubscribeGoesAcrossNestsAtTheHome() {
        val api = mock(ApiClient::class.java)
        runBlocking {
            whenever(api.subscriptionSubscribe(anyString(), anyString(), anyString()))
                .thenReturn(FfiSubscribeReply.Queued(requestId = 1L))
        }
        val vm = ProfileOffersVM(api)

        vm.subscribe(author, "gold", remote)

        val subscribes = calls(api).filter { it.first == "subscriptionSubscribe" }
        assertEquals(listOf(listOf<Any?>(author, "gold", remote)), subscribes.map { it.second.take(3) })
        assertEquals(setOf("gold"), vm.pendingTiers.value)
    }
}
