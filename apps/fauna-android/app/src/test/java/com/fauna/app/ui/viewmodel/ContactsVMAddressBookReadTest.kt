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
import com.fauna.ffi.FfiAddressbooksRead
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.flow.emptyFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * The Address Book's book read on android (contacts.md § Errors & edge cases;
 * mail-credentials.md § MSEK lifecycle, ruling 7): while the read is unresolved
 * — custody not readable yet, or no key minted yet (the mint in flight) — the
 * book picker paints loading, never an empty address book, and the
 * store-change notice re-runs the read until it answers. FFI-free: [ApiClient]
 * is mocked as in [EventsVMStoreChangeTest].
 */
@OptIn(ExperimentalCoroutinesApi::class)
class ContactsVMAddressBookReadTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private class Harness(
        val vm: ContactsVM,
        val api: ApiClient,
        val storeChangedTick: MutableSharedFlow<Unit>,
    )

    private fun harness(): Harness {
        val api = mock(ApiClient::class.java)
        val storage = mock(SecureStorage::class.java)
        val contactDao = mock(ContactDao::class.java)
        val knockDao = mock(KnockDao::class.java)
        val aftermath = mock(SuccessionAftermathHost::class.java)
        val storeChangedTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        whenever(storage.secretHex).thenReturn("11".repeat(32))
        whenever(api.wardAsks).thenReturn(
            WardAsks().apply { setFromStatus(false, null, emptyList(), emptyList()) },
        )
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        whenever(api.knockTick).thenReturn(MutableSharedFlow())
        whenever(api.addressBookChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.storeChangedTick).thenReturn(storeChangedTick)
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
        return Harness(vm, api, storeChangedTick)
    }

    @Test
    fun anUnresolvedReadPaintsLoadingNeverTheEmptyBook() = runTest {
        val h = harness()
        whenever(h.api.readAddressbooks()).thenReturn(FfiAddressbooksRead.Unresolved)

        h.vm.showAddressBook()

        assertEquals(FfiAddressbooksRead.Unresolved, h.vm.addressbooksRead.value)
        assertEquals(emptyList<Any>(), h.vm.addressbooks.value)
        assertEquals(AddressBookPlaceholder.LOADING, addressBookPlaceholder(h.vm.addressbooksRead.value))
    }

    @Test
    fun aStoreChangeReRunsAnUnresolvedReadUntilItAnswers() = runTest {
        val h = harness()
        whenever(h.api.readAddressbooks())
            .thenReturn(FfiAddressbooksRead.Unresolved)
            .thenReturn(FfiAddressbooksRead.Loaded(emptyList()))
        h.vm.showAddressBook()

        h.storeChangedTick.tryEmit(Unit)

        verify(h.api, times(2)).readAddressbooks()
        assertEquals(AddressBookPlaceholder.EMPTY, addressBookPlaceholder(h.vm.addressbooksRead.value))

        // A settled read is the push's to refresh, not the store's.
        h.storeChangedTick.tryEmit(Unit)
        verify(h.api, times(2)).readAddressbooks()
    }

    @Test
    fun aStoreChangeOffTheSegmentReadsNothing() = runTest {
        val h = harness()

        h.storeChangedTick.tryEmit(Unit)

        verify(h.api, times(0)).readAddressbooks()
    }

    @Test
    fun onlyALoadedReadWithNoBookIsTheEmptyAddressBook() {
        assertEquals(AddressBookPlaceholder.LOADING, addressBookPlaceholder(FfiAddressbooksRead.Unresolved))
        assertEquals(AddressBookPlaceholder.EMPTY, addressBookPlaceholder(FfiAddressbooksRead.Loaded(emptyList())))
    }
}
