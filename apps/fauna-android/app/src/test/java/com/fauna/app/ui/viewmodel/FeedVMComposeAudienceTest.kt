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
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertFalse
import org.junit.Before
import org.junit.Test
import org.mockito.ArgumentMatchers.any
import org.mockito.ArgumentMatchers.anyString
import org.mockito.Mockito.doAnswer
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_feed.FeedComposeState
import uniffi.fauna_feed.FeedSnapshot
import uniffi.fauna_feed.SellComposeState

/**
 * The composer's audience lives in the shared manager, never in the screen
 * (`docs/goal/ui/feed.md` § Persistence → *Only user-authored input rests*:
 * `gate_tier`, `gate_preview`, `sell` and `gate_room` survive a restart; § User
 * actions → `post-submit-button`). A draft restored gated to a tier must post
 * gated or be refused — never re-staged as Public by the Post click and
 * published to everyone. linux, apple and tui share the shape: picks forward to
 * the manager as they are made, and submit reads the manager's audience.
 *
 * The manager is a small stateful fake over a mock: [stagedTier] is what
 * `update_compose_gate` last staged, and `prepare_gated_blob` answers as the
 * shared manager does — `null` (the plain, public path) for an ungated compose,
 * the fail-closed `feed.compose_gate_no_key` refusal for a tier this device
 * holds no key for (`ownTiers` empty after a restart). The shared refusal
 * itself is pinned in `libs/fauna-feed`.
 */
@ExperimentalCoroutinesApi
class FeedVMComposeAudienceTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private class Harness(val vm: FeedVM, val manager: FfiFeedManager)

    /** The tier the fake manager holds staged right now (`null` = Public). */
    private var stagedTier: String? = null

    private fun harness(restored: FeedComposeState): Harness {
        stagedTier = restored.gateTier
        val manager = mock(FfiFeedManager::class.java)
        val snapshot = mock(FeedSnapshot::class.java)
        whenever(snapshot.compose).thenReturn(restored)
        whenever(manager.snapshot()).thenReturn(snapshot)
        doAnswer { stagedTier = it.arguments[0] as String?; null }
            .`when`(manager).updateComposeGate(any(), anyString())
        runBlocking {
            whenever(manager.prepareGatedBlob()).thenAnswer {
                if (stagedTier == null) null
                else throw RuntimeException("feed.compose_gate_no_key")
            }
        }
        val host = mock(FeedManagerHost::class.java)
        whenever(host.manager()).thenReturn(manager)
        val api = mock(ApiClient::class.java)
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        val vm = FeedVM(
            host,
            api,
            mock(SecureStorage::class.java),
            mock(ContentPolicyStore::class.java),
            mock(FamilyNotifyStore::class.java),
            mock(WebPublishStore::class.java),
        )
        return Harness(vm, manager)
    }

    private fun compose(
        gateTier: String? = null,
        gatePreview: String = "",
        sell: SellComposeState? = null,
        gateRoom: String? = null,
    ) = FeedComposeState(
        text = "the full body",
        tags = "",
        attachedFile = null,
        gateTier = gateTier,
        gatePreview = gatePreview,
        sell = sell,
        gateRoom = gateRoom,
        error = null,
        submitting = false,
    )

    /** Names of every audience setter the VM called on the manager. */
    private fun audienceWrites(m: FfiFeedManager): List<String> =
        mockingDetails(m).invocations.map { it.method.name }.filter {
            it in setOf("updateComposeGate", "updateComposeRoom", "updateComposeSell", "updateComposePreview")
        }

    @Test
    fun aRestoredTierDraftIsRefusedNeverPublishedPublic() {
        val h = harness(compose(gateTier = "Gold", gatePreview = "a teaser"))

        val ok = runBlocking { h.vm.submitPost("the full body", "") }

        // The Post click submits the manager's audience: the restored tier
        // stays staged and the shared manager refuses it (no key for "Gold"
        // with ownTiers empty) — the post never takes the public path.
        assertFalse(ok)
        assertEquals("Gold", stagedTier)
        assertEquals(emptyList<String>(), audienceWrites(h.manager))
        runBlocking { verify(h.manager, never()).submitPost() }
    }

    @Test
    fun aRestoredSaleSubmitsTheManagersSale() {
        val sale = SellComposeState(price = "5 sats", askingPrice = "", subscribersGetItFree = false)
        val h = harness(compose(gatePreview = "a teaser", sell = sale))

        runBlocking { h.vm.submitPost("the full body", "") }

        // The sale comes off the manager — never re-staged, and its knobs are
        // the restored ones, not a page-local default.
        assertEquals(emptyList<String>(), audienceWrites(h.manager))
        runBlocking { verify(h.manager).prepareSellPost("5 sats", false, null) }
        runBlocking { verify(h.manager, never()).prepareGatedBlob() }
    }

    @Test
    fun aTierPickForwardsKeepingTheManagersTeaser() {
        val h = harness(compose(gatePreview = "a teaser"))

        h.vm.setComposeGate("Gold")

        verify(h.manager).updateComposeGate("Gold", "a teaser")
    }

    @Test
    fun aRoomPickForwardsKeepingTheManagersTeaser() {
        val h = harness(compose(gatePreview = "a teaser"))

        h.vm.setComposeRoom("ab".repeat(32))

        verify(h.manager).updateComposeRoom("ab".repeat(32), "a teaser")
    }

    @Test
    fun aTeaserEditTouchesNoAudienceAnswer() {
        val h = harness(compose(gateRoom = "ab".repeat(32)))

        h.vm.setComposePreview("new teaser")

        assertEquals(listOf("updateComposePreview"), audienceWrites(h.manager))
        verify(h.manager).updateComposePreview("new teaser")
    }
}
