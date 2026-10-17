package com.fauna.app.ui.viewmodel

import android.content.Context
import androidx.lifecycle.SavedStateHandle
import com.fauna.app.core.ApiClient
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
 * The cross-nest knock seam (contacts.md § Implementation status today →
 * *Cross-nest knocks*): the nest a Find User lookup resolved on
 * ([ResolveService.ResolvedRecipient.nestUrl]) travels with the knock as
 * `sendKnock`'s `recipientNestUrl`, carried from the lookup and never
 * re-derived; a home-nest result carries `null`.
 *
 * Collaborators are mocks; `actorIdFromSecret` is the real shared Rust over
 * UniFFI, so this runs under `just android-host-test`.
 */
@ExperimentalCoroutinesApi
class ContactsVMCrossNestKnockTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val peer = "cd".repeat(32)
    private val peerNest = "https://peer.example"

    private class Harness(val vm: ContactsVM, val api: ApiClient)

    private fun harness(): Harness {
        val api = mock(ApiClient::class.java)
        val storage = mock(SecureStorage::class.java)
        val contactDao = mock(ContactDao::class.java)
        val knockDao = mock(KnockDao::class.java)
        val aftermath = mock(SuccessionAftermathHost::class.java)
        whenever(storage.secretHex).thenReturn("11".repeat(32))
        whenever(api.wardAsks).thenReturn(WardAsks())
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.knockTick).thenReturn(MutableSharedFlow())
        // The VM's init collects this; an unstubbed mock returns null and the
        // collector's NPE would leak into a later class's test scope.
        whenever(api.addressBookChangedTick).thenReturn(MutableSharedFlow())
        whenever(aftermath.configStageSettled).thenReturn(MutableSharedFlow())
        whenever(contactDao.getAll()).thenReturn(emptyFlow())
        whenever(knockDao.getAll()).thenReturn(emptyFlow())
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
        return Harness(vm, api)
    }

    /** The `(recipient, recipientNestUrl)` of every knock the VM issued. */
    private fun knocks(api: ApiClient): List<Pair<Any?, Any?>> =
        mockingDetails(api).invocations
            .filter { it.method.name == "sendKnock" }
            .map { it.arguments[1] to it.arguments[2] }

    @Test
    fun aForeignLookupsNestReachesTheKnock() {
        val h = harness()
        h.vm.addContact(peer, peerNest)
        assertEquals(listOf<Pair<Any?, Any?>>(peer to peerNest), knocks(h.api))
    }

    @Test
    fun aHomeNestLookupKnocksWithNoRecipientNest() {
        val h = harness()
        h.vm.addContact(peer)
        assertEquals(listOf<Pair<Any?, Any?>>(peer to null), knocks(h.api))
    }
}
