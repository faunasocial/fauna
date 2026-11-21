package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.events.EventDraftsHost
import com.fauna.ffi.FfiRefusedChange
import com.fauna.ffi.FfiCalendarsRead
import kotlinx.coroutines.Dispatchers
import kotlinx.coroutines.ExperimentalCoroutinesApi
import kotlinx.coroutines.flow.MutableSharedFlow
import kotlinx.coroutines.test.UnconfinedTestDispatcher
import kotlinx.coroutines.test.resetMain
import kotlinx.coroutines.test.runTest
import kotlinx.coroutines.test.setMain
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.times
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever

/**
 * The store-change notice on the Events screen (account-runtime.md §
 * Multi-instance concurrency → *A runtime's own pump is a source of the notice
 * too*, parts 2 and 4; events.md § Refused scheduling changes → *While the page
 * is open*): an OPEN page re-reads the refused-change list — the list ONLY —
 * when [ApiClient.storeChangedTick] fires, because a refusal is a background
 * write of this runtime. The calendars and events beside it are the nest's, not
 * the store's — save while the calendar read is unresolved, when the notice
 * re-runs the whole load (events.md § No calendar to hold an event, ruling 5). FFI-free: [ApiClient] is mocked as in [DevicesVMTest], and the
 * ticks are stubbed before construction because `init` collects them.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class EventsVMStoreChangeTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private data class Fixture(
        val vm: EventsVM,
        val api: ApiClient,
        val storeChangedTick: MutableSharedFlow<Unit>,
    )

    private fun makeVm(): Fixture {
        val api = mock(ApiClient::class.java)
        val storeChangedTick = MutableSharedFlow<Unit>(replay = 0, extraBufferCapacity = 1)
        whenever(api.storeChangedTick).thenReturn(storeChangedTick)
        whenever(api.calendarChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        val drafts = mock(EventDraftsHost::class.java)
        return Fixture(EventsVM(api, drafts), api, storeChangedTick)
    }

    /** While the calendar read is unresolved (custody not readable yet, or the
     *  key's mint in flight) the notice re-runs the whole load, so the key's
     *  landing reaches the page (`ui/events.md` § *No calendar to hold an
     *  event*, ruling 5). */
    @Test
    fun aStoreChangeReRunsTheLoadWhileTheCalendarReadIsUnresolved() = runTest {
        val (_, api, storeChangedTick) = makeVm()
        whenever(api.readCalendars()).thenReturn(FfiCalendarsRead.Unresolved)

        storeChangedTick.tryEmit(Unit)

        verify(api, times(1)).readCalendars()
    }

    @Test
    fun aStoreChangeTickReReadsTheRefusedListAndNothingElse() = runTest {
        val (vm, api, storeChangedTick) = makeVm()
        // A settled read: the calendars are the nest's, not the store's.
        vm.calendarsRead.value = FfiCalendarsRead.Loaded(emptyList(), 0u)
        val refused = mock(FfiRefusedChange::class.java)
        whenever(api.refusedChangesOpen()).thenReturn(listOf(refused))

        storeChangedTick.tryEmit(Unit)

        assertEquals(listOf(refused), vm.refusedChanges.value)
        verify(api, times(1)).refusedChangesOpen()
        verify(api, never()).readCalendars()
        verify(api, never()).queryMyEvents("invited")
    }

    @Test
    fun aFailedReReadKeepsThePaintedList() = runTest {
        val (vm, api, storeChangedTick) = makeVm()
        vm.calendarsRead.value = FfiCalendarsRead.Loaded(emptyList(), 0u)
        val refused = mock(FfiRefusedChange::class.java)
        whenever(api.refusedChangesOpen())
            .thenReturn(listOf(refused))
            .thenThrow(RuntimeException("store unreadable"))
        storeChangedTick.tryEmit(Unit)
        assertEquals(listOf(refused), vm.refusedChanges.value)

        storeChangedTick.tryEmit(Unit)

        verify(api, times(2)).refusedChangesOpen()
        assertEquals(listOf(refused), vm.refusedChanges.value)
    }
}
