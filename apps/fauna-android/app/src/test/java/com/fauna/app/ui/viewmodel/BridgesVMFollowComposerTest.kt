package com.fauna.app.ui.viewmodel

import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.ApiClient
import com.fauna.app.core.WardAsks
import com.fauna.app.testing.FaunaRobolectricTestRunner
import com.fauna.ffi.FfiBridgeFollow
import com.fauna.ffi.FfiBridgeStatus
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNull
import org.junit.Before
import org.junit.Test
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import org.robolectric.annotation.Config

/**
 * [BridgesVM.addFollow] / [BridgesVM.removeFollow] through the shared
 * `OfflineQueued` composer (`account-offline-mutation.md` § The
 * offline-mutation contract): with no nest the FFI doors
 * (`addFollowOrQueue` / `removeFollowOrQueue`) queue the change and nothing is
 * re-read — the bridges page paints only what the nest persisted; connected,
 * the change is sent and the list is re-read as before.
 */
@OptIn(ExperimentalCoroutinesApi::class)
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], application = android.app.Application::class)
class BridgesVMFollowComposerTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val context = ApplicationProvider.getApplicationContext<android.app.Application>()

    /** The VM with both doors answering [queued] for `nostr` / `npub1alice`. */
    private fun make(queued: Boolean): Pair<BridgesVM, ApiClient> = runBlocking {
        val api = mock(ApiClient::class.java)
        whenever(api.wardAsks).thenReturn(WardAsks())
        whenever(api.listBridges()).thenReturn(emptyList<FfiBridgeStatus>())
        whenever(api.addBridgeFollowOrQueue("nostr", "npub1alice", null, null)).thenReturn(queued)
        whenever(api.removeBridgeFollowOrQueue("nostr", "npub1alice")).thenReturn(queued)
        BridgesVM(api, context) to api
    }

    @Test
    fun aFollowAndUnfollowWithNoNestAreQueuedAndChangeNoList() = runBlocking<Unit> {
        val (vm, api) = make(queued = true)
        vm.addFollow("nostr", "npub1alice", null)
        vm.removeFollow("nostr", "npub1alice")
        verify(api).addBridgeFollowOrQueue("nostr", "npub1alice", null, null)
        verify(api).removeBridgeFollowOrQueue("nostr", "npub1alice")
        verify(api, never()).listBridges()
        assertEquals(emptyMap<String, List<FfiBridgeFollow>>(), vm.follows.value)
        assertNull(vm.error.value)
    }

    @Test
    fun aFollowWithTheNestConnectedIsSentAndReListed() = runBlocking<Unit> {
        val (vm, api) = make(queued = false)
        vm.addFollow("nostr", "npub1alice", null)
        verify(api).listBridges()
        assertNull(vm.error.value)
    }
}
