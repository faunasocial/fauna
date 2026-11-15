package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.ContentPolicyStore
import com.fauna.app.core.FamilyNotifyStore
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.WebPublishStore
import com.fauna.app.core.feed.FeedManagerHost
import com.fauna.ffi.FfiFeedManager
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_feed.FeedComposeState
import uniffi.fauna_feed.FeedSnapshot

/**
 * The sold post's payment link (`gated-post-payment-link`; `monetization.md`
 * § Pillar 3 → *Buyer↔actor binding (Q4)*, the Q13 rules (1) and (2)): the
 * gesture runs the shared manager's `pay_unlock_offer` — which subscribes to
 * the offer's tier FIRST and only then composes the checkout URL — and opens
 * exactly the URL it returns, never the author's raw link. `null` opens
 * nothing; a refused subscribe lands on [FeedVM.buyUnlockError] (the screen
 * renders it in the `feed_error_buy_unlock` template) and opens nothing. Lifts
 * tui's `Op::PayUnlockOffer`; the subscribe-then-compose order itself is
 * pinned in `libs/fauna-feed`.
 */
@ExperimentalCoroutinesApi
class FeedVMPayUnlockOfferTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val postId = "post-1"
    private val composed = "https://buy.stripe.com/unlock?client_reference_id=xyz"

    private class Harness(val vm: FeedVM, val manager: FfiFeedManager)

    private fun harness(pay: () -> String?): Harness {
        val manager = mock(FfiFeedManager::class.java)
        val snapshot = mock(FeedSnapshot::class.java)
        whenever(snapshot.compose).thenReturn(
            FeedComposeState(
                text = "",
                tags = "",
                attachedFile = null,
                gateTier = null,
                gatePreview = "",
                sell = null,
                gateRoom = null,
                error = null,
                submitting = false,
            ),
        )
        whenever(manager.snapshot()).thenReturn(snapshot)
        runBlocking { whenever(manager.payUnlockOffer(postId)).thenAnswer { pay() } }
        val host = mock(FeedManagerHost::class.java)
        whenever(host.manager()).thenReturn(manager)
        val api = mock(ApiClient::class.java)
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        val vm = FeedVM(
            host,
            api,
            mock(SecureStorage::class.java),
            policyStoreWithNoSpamMarks(),
            mock(FamilyNotifyStore::class.java),
            mock(WebPublishStore::class.java),
        )
        return Harness(vm, manager)
    }

    /** FeedVM's init collects the store's spam marks; a bare mock answers null and
     *  the collector's NPE leaks into whichever test class runs next. */
    private fun policyStoreWithNoSpamMarks(): ContentPolicyStore {
        val store = mock(ContentPolicyStore::class.java)
        whenever(store.spamMarked).thenReturn(MutableStateFlow(emptyList()))
        return store
    }

    @Test
    fun thePaymentLinkOpensTheCheckoutTheSharedManagerComposed() {
        val opened = mutableListOf<String>()
        val h = harness { composed }

        h.vm.payUnlockOffer(postId) { opened += it }

        runBlocking { verify(h.manager).payUnlockOffer(postId) }
        assertEquals(listOf(composed), opened)
        assertNull(h.vm.buyUnlockError.value)
    }

    @Test
    fun nothingToOpenOpensNothing() {
        val opened = mutableListOf<String>()
        val h = harness { null }

        h.vm.payUnlockOffer(postId) { opened += it }

        assertEquals(emptyList<String>(), opened)
        assertNull(h.vm.buyUnlockError.value)
    }

    @Test
    fun aRefusedSubscribeIsTheBuyErrorAndOpensNothing() {
        val opened = mutableListOf<String>()
        val h = harness { throw RuntimeException("subscribe refused") }

        h.vm.payUnlockOffer(postId) { opened += it }

        assertEquals(emptyList<String>(), opened)
        assertEquals("subscribe refused", h.vm.buyUnlockError.value)
    }
}
