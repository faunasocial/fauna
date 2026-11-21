package com.fauna.app.ui.viewmodel

import com.fauna.app.core.ApiClient
import com.fauna.app.core.events.EventDraftsHost
import com.fauna.app.data.api.CreateEventRequest
import com.fauna.ffi.FfiCalendarRow
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
import org.junit.Assert.assertFalse
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Before
import org.junit.Test
import org.mockito.Mockito.mock
import org.mockito.Mockito.never
import org.mockito.Mockito.verify
import org.mockito.Mockito.`when` as whenever
import uniffi.fauna_core.Possibility

/**
 * Events with no calendar to hold an event (`ui/events.md` § Errors & edge
 * cases → *No calendar to hold an event*): the page keeps the shared four-answer
 * read (ruling 2), every door asks the shared `event_create_possibility` over it
 * and `create-event` asks `event_create_target` before it sends (ruling 3), and
 * a refused or failed save is the form's own error with the form kept open
 * (ruling 4). The predicate and the target are shared Rust, read through
 * UniFFI; [ApiClient] is mocked.
 */
@OptIn(ExperimentalCoroutinesApi::class)
class EventsVMCreateGateTest {

    @Before
    fun installMainDispatcher() = Dispatchers.setMain(UnconfinedTestDispatcher())

    @After
    fun removeMainDispatcher() = Dispatchers.resetMain()

    private val personal = FfiCalendarRow(id = "ab".repeat(32), name = "Personal", color = "")

    private val request = CreateEventRequest(
        calendarId = "",
        summary = "Standup",
        dtstart = "2026-11-18T09:00:00",
        dtend = "2026-11-18T10:00:00",
        description = null,
        location = null,
    )

    private fun makeVm(): Pair<EventsVM, ApiClient> {
        val api = mock(ApiClient::class.java)
        whenever(api.storeChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.calendarChangedTick).thenReturn(MutableSharedFlow())
        whenever(api.reconnectTick).thenReturn(MutableSharedFlow())
        return EventsVM(api, mock(EventDraftsHost::class.java)) to api
    }

    /** Before the first read lands the page is unresolved: the doors are
     *  blocked "loading", never the empty state's answer. */
    @Test
    fun beforeTheFirstReadEveryDoorIsBlockedLoading() {
        val (vm, _) = makeVm()
        assertEquals(FfiCalendarsRead.Unresolved, vm.calendarsRead.value)
        val gate = vm.createGate.value
        assertTrue("blocked while unresolved, got $gate", gate is Possibility.Blocked)
        assertEquals("events.create_blocked.loading", (gate as Possibility.Blocked).reason.key)
    }

    /** A loaded read opens every door and lists the calendars. */
    @Test
    fun aLoadedReadOpensTheDoors() = runTest {
        val (vm, api) = makeVm()
        whenever(api.readCalendars()).thenReturn(FfiCalendarsRead.Loaded(listOf(personal), 0u))
        vm.load()
        assertEquals(Possibility.Possible, vm.createGate.value)
        assertEquals(listOf("Personal"), vm.calendars.value.map { it.name })
    }

    /** A refused lazy `Personal` blocks the doors with its reason, which the
     *  empty state paints. */
    @Test
    fun aProvisionFailureBlocksWithItsReason() = runTest {
        val (vm, api) = makeVm()
        whenever(api.readCalendars()).thenReturn(FfiCalendarsRead.ProvisionFailed("refused"))
        vm.load()
        val gate = vm.createGate.value as Possibility.Blocked
        assertEquals("events.create_blocked.provision_failed", gate.reason.key)
        assertTrue(vm.calendars.value.isEmpty())
    }

    /** `create-event` asks the same predicate: unresolved, it refuses with the
     *  reason on the form and nothing reaches the wire. */
    @Test
    fun theSubmitRefusesWhileUnresolved_andSendsNothing() = runTest {
        val (vm, api) = makeVm()
        var saved = false
        vm.createEvent(request) { saved = true }
        val error = vm.eventFormError.value
        assertTrue("refused on the form, got $error", error is EventsVM.EventFormError.Refused)
        assertEquals("events.create_blocked.loading", (error as EventsVM.EventFormError.Refused).reason.key)
        assertFalse(saved)
        verify(api, never()).createEvent(request)
    }

    /** A save that fails after the gate is the form's own error and the form
     *  stays open (the close never runs). */
    @Test
    fun aFailedSaveKeepsTheFormOpenWithTheSentence() = runTest {
        val (vm, api) = makeVm()
        whenever(api.readCalendars()).thenReturn(FfiCalendarsRead.Loaded(listOf(personal), 0u))
        vm.load()
        whenever(api.createEvent(request.copy(calendarId = personal.id))).thenThrow(RuntimeException("nest refused"))
        var saved = false
        vm.createEvent(request) { saved = true }
        assertEquals(EventsVM.EventFormError.SaveFailed("nest refused"), vm.eventFormError.value)
        assertFalse(saved)
    }

    /** A landed save targets the loaded calendar and closes the form. */
    @Test
    fun aLandedSaveTargetsTheCalendarAndCloses() = runTest {
        val (vm, api) = makeVm()
        whenever(api.readCalendars()).thenReturn(FfiCalendarsRead.Loaded(listOf(personal), 0u))
        vm.load()
        var saved = false
        vm.createEvent(request) { saved = true }
        assertTrue(saved)
        assertNull(vm.eventFormError.value)
        verify(api).createEvent(request.copy(calendarId = personal.id))
    }
}
