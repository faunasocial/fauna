package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.SavedStateHandle
import com.fauna.app.core.ApiClient
import com.fauna.app.core.HexUtil
import com.fauna.ffi.actorIdFromSecret
import com.fauna.app.core.ResolveService
import com.fauna.app.core.SecureStorage
import com.fauna.app.core.SuccessionAftermathHost
import com.fauna.app.core.WardAsks
import com.fauna.app.data.db.ContactDao
import com.fauna.app.data.db.KnockDao
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.runBlocking
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.mockingDetails
import org.mockito.Mockito.`when` as whenever

/**
 * The knock accept's `OfflineQueued` routing (`account-offline-mutation.md`
 * § Implementation status today, the knock-accept and abuse-report composers):
 * with no nest the FFI door (`knocksAcceptOrQueue`) answers QUEUED, the knock
 * leaves the local list and nothing is re-read (no roster row is added); sent,
 * the knocks and roster are re-read. The door's own send-or-queue is pinned
 * Rust-side (`fauna-ffi`'s `a_knock_accept_with_no_nest_is_queued`).
 *
 * Collaborators are mocks; `actorIdFromSecret` is the real shared Rust over
 * UniFFI, so this runs under `just android-host-test`.
 */
@ExperimentalCoroutinesApi
class ContactsVMKnockAcceptTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val peer = "cd".repeat(32)

    private class Harness(val vm: ContactsVM, val api: ApiClient, val knockDao: KnockDao)

    private fun harness(queued: Boolean): Harness {
        val api = mock(ApiClient::class.java)
        val storage = mock(SecureStorage::class.java)
        val contactDao = mock(ContactDao::class.java)
        val knockDao = mock(KnockDao::class.java)
        val aftermath = mock(SuccessionAftermathHost::class.java)
        whenever(storage.secretHex).thenReturn("11".repeat(32))
        whenever(api.wardAsks).thenReturn(WardAsks())
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.knockTick).thenReturn(MutableSharedFlow())
        whenever(api.addressBookChangedTick).thenReturn(MutableSharedFlow())
        whenever(aftermath.configStageSettled).thenReturn(MutableSharedFlow())
        whenever(contactDao.getAll()).thenReturn(emptyFlow())
        whenever(knockDao.getAll()).thenReturn(emptyFlow())
        runBlocking {
            val actorId = HexUtil.bytesToHex(actorIdFromSecret(HexUtil.hexToBytes("11".repeat(32))))
            whenever(api.acceptKnockOrQueue(actorId, peer))
                .thenReturn(queued)
        }
        val vm = ContactsVM(
            SavedStateHandle(),
            api,
            storage,
            contactDao,
            knockDao,
            mock(ResolveService::class.java),
            aftermath,
            mock(Context::class.java),
        )
        return Harness(vm, api, knockDao)
    }

    private fun calls(mock: Any, name: String): Int =
        mockingDetails(mock).invocations.count { it.method.name == name }

    @Test
    fun aKnockAcceptWithNoNestIsQueuedAndLeavesTheLocalList() {
        val h = harness(queued = true)
        val reads = calls(h.api, "fetchKnocks")
        h.vm.acceptKnock(peer)
        val dropped = mockingDetails(h.knockDao).invocations
            .filter { it.method.name == "deleteBySender" }
            .map { it.arguments[0] }
        assertEquals(listOf<Any?>(peer), dropped)
        assertEquals("queued: never re-read", reads, calls(h.api, "fetchKnocks"))
    }

    @Test
    fun aKnockAcceptWithTheNestConnectedIsSentNotQueued() {
        val h = harness(queued = false)
        val reads = calls(h.api, "fetchKnocks")
        h.vm.acceptKnock(peer)
        assertEquals("sent: nothing dropped locally", 0, calls(h.knockDao, "deleteBySender"))
        assertEquals("sent: the knocks are re-read", reads + 1, calls(h.api, "fetchKnocks"))
    }
}
