package com.fauna.app.ui.viewmodel

import androidx.lifecycle.ViewModel
import androidx.lifecycle.viewModelScope
import com.fauna.app.core.ApiClient
import com.fauna.app.data.api.*
import com.fauna.ffi.calendarIsDisplayed
import com.fauna.ffi.resolveCalendarSelection
import dagger.hilt.android.lifecycle.HiltViewModel
import kotlinx.coroutines.async
import kotlinx.coroutines.flow.MutableStateFlow
import kotlinx.coroutines.launch
import javax.inject.Inject
import com.fauna.app.ui.screen.events.localeFirstDayOfWeek
import com.fauna.app.ui.screen.events.pannedBy
import com.fauna.app.ui.screen.events.snapToWeekStart
import com.fauna.ffi.FfiCalendarViewMode
import uniffi.fauna_core.RsvpResponse
import java.time.DayOfWeek
import java.time.LocalDate
import java.time.YearMonth

@HiltViewModel
class EventsVM @Inject constructor(
    private val api: ApiClient,
    // Draft-persistence v2, events rail (reserved-folders.md § Drafts Sync;
    // events.md § Persistence) — process-wide, survives this VM being
    // recreated on navigation, which is what makes the rail (not the compose
    // surface) the live draft's owner.
    private val draftsHost: com.fauna.app.core.events.EventDraftsHost,
) : ViewModel() {

    /** The live events-rail draft, or `null` for "nothing to resume" —
     *  reactive so the New Event sheet picks up a restore that lands after
     *  it is already open (the late-restore rule). */
    val draft = draftsHost.draft

    /** Every `event-form` field edit — updates the live draft immediately and
     *  re-arms the debounced upload (the fourth rule). */
    fun onDraftEdited(
        summary: String, dtstart: String, dtend: String, description: String, location: String,
    ) = draftsHost.onEdit(summary, dtstart, dtend, description, location)

    /** Empty the rail — a day-cell "start fresh" open, or (via [createEvent])
     *  a successful create. */
    fun clearDraft() = draftsHost.clear()

    val calendars = MutableStateFlow<List<FaunaCalendar>>(emptyList())
    val selectedCalendar = MutableStateFlow<FaunaCalendar?>(null)
    // The `calendar-visibility` display filter's checked set (events.md §
    // Where logic lives → *Which calendars display*) — client-side only,
    // never persisted server-side. An **empty** set means "no filter" (the
    // full union); `seedVisibleCalendarIds` fills it on every calendar-list
    // load so a fresh page paints every box checked rather than leaning on
    // that rule (mirrors tui/linux/web).
    val visibleCalendarIds = MutableStateFlow<Set<String>>(emptySet())
    val events = MutableStateFlow<List<EventSummary>>(emptyList())
    val invitedEvents = MutableStateFlow<List<EventSummary>>(emptyList())
    val selectedEvent = MutableStateFlow<EventDetail?>(null)
    val selectedEventAttendees = MutableStateFlow<List<Attendee>>(emptyList())
    val currentReminder = MutableStateFlow<String?>(null)
    val isLoading = MutableStateFlow(false)
    val errorMessage = MutableStateFlow<String?>(null)

    // The just-created calendar, so the next authoring action can target it from
    // the no-selection union view without a "pick a calendar" step (events.md
    // § Layout & flow). Captured by diffing the calendar list across a create,
    // since the create RPC returns no id.
    private var lastCreatedCalendarId: String? = null

    // Monotonic guards: a slow no-selection union query (which fans out over
    // every calendar) must never overwrite a fresher single-calendar result that
    // started later — the rapid create→select→create-event sequence otherwise
    // leaves the last-resolving stale query winning (the race web hit; parity
    // keeps the guard). `agendaGen` guards `events`, `rangeGen` guards
    // `visibleEvents`.
    private var agendaGen = 0
    private var rangeGen = 0

    // Unfiltered fetch results — `events`/`visibleEvents` are these re-run
    // through `calendarIsDisplayed` on every fetch AND every visibility
    // toggle, so toggling a box is purely local (no refetch: the raw cache
    // already holds every owned calendar's events).
    private var rawEvents: List<EventSummary> = emptyList()
    private var rawVisibleEvents: List<EventSummary> = emptyList()

    /** [rawEvents]/[rawVisibleEvents] is re-filtered here rather than in
     *  `scopedEventItems`, since a single-calendar selection query never
     *  fetches the other calendars visibility would need to hide/show it
     *  against — the shared predicate resolves that scope composition
     *  itself from `selectedCalendar` + `calendars` + `visibleCalendarIds`. */
    private fun displayed(raw: List<EventSummary>): List<EventSummary> =
        displayedEvents(raw, selectedCalendar.value, calendars.value, visibleCalendarIds.value)

    /** Flip one calendar's `calendar-visibility` checkbox — purely local
     *  display state; re-filters the already-fetched raw caches, no refetch. */
    fun toggleCalendarVisibility(calendarId: String) {
        val next = visibleCalendarIds.value.toMutableSet()
        if (!next.remove(calendarId)) next.add(calendarId)
        visibleCalendarIds.value = next
        events.value = displayed(rawEvents)
        visibleEvents.value = displayed(rawVisibleEvents)
    }

    /** The calendar an authoring action targets when the user hasn't explicitly
     *  selected one: the just-created calendar, else the first available — so an
     *  event can be added straight from the no-selection union view (events.md
     *  § Layout and flow; mirrors web's `activeCalendar` / apple's
     *  `selectedCalendar ?? calendars.first`). */
    fun activeCalendar(): FaunaCalendar? =
        selectedCalendar.value
            ?: lastCreatedCalendarId?.let { id -> calendars.value.find { it.id == id } }
            ?: calendars.value.firstOrNull()

    init {
        // `fauna.calendar.changed` — a durable write landed in one of this
        // actor's calendars (own other device, or an external MUA via the MDA).
        // The push is a nudge: re-list calendars and re-query the visible
        // selection (transport.md § Push events; same tick idiom as
        // NotificationsVM.notificationTick).
        viewModelScope.launch {
            api.calendarChangedTick.collect { refreshFromPush() }
        }
        // Reconnect backstop: a push fired while the socket was down is never
        // replayed, so a mounted Events screen must re-pull on reconnect too —
        // `StaleSurfaces::on_reconnect()` stales `events` along with every other
        // surface (`transport.md` § Which surfaces a push invalidates). This VM
        // had no reconnect arm before adopting the shared classifier (the same
        // class of gap the seam's own audit found on web's Events page).
        viewModelScope.launch {
            api.reconnectTick.collect { refreshFromPush() }
        }
    }

    private fun refreshFromPush() {
        // Re-list calendars FIRST (a newly-appeared external calendar must join
        // the no-selection union), then re-query the current scope (selected
        // calendar or the union). Mirrors web's push arm (events.md dated history
        // 2026-07-18). The push is a hint; errors surface on the next explicit load.
        viewModelScope.launch {
            try {
                val previous = calendars.value
                calendars.value = api.listCalendars()
                visibleCalendarIds.value = seedVisibleCalendarIds(visibleCalendarIds.value, previous, calendars.value)
            } catch (_: Exception) {
            }
            refreshEvents()
            loadEventsForCurrentView()
        }
    }

    /** Mount / pull-to-refresh entry: re-list calendars + invited events, then
     *  render the current scope (selected calendar or the no-selection union).
     *  Calendars must be listed before the union can fan out over them. */
    fun load() {
        viewModelScope.launch {
            try {
                val previous = calendars.value
                calendars.value = api.listCalendars()
                visibleCalendarIds.value = seedVisibleCalendarIds(visibleCalendarIds.value, previous, calendars.value)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            refreshEvents()
            loadEventsForCurrentView()
        }
        loadInvitedEvents()
    }

    /** Load the agenda list for the current scope: the selected calendar's
     *  events, or — with none selected — the no-selection union of every owned
     *  calendar (events.md § Implementation status, 2026-07-18). The `agendaGen`
     *  guard drops a stale union result that resolves after a fresher load. */
    fun refreshEvents() {
        val gen = ++agendaGen
        viewModelScope.launch {
            isLoading.value = true
            try {
                val evs = scopedEventItems(selectedCalendar.value, calendars.value) {
                    api.queryEvents(it)
                }
                if (gen != agendaGen) return@launch
                rawEvents = evs
                events.value = displayed(rawEvents)
            } catch (e: Exception) {
                if (gen == agendaGen) {
                    errorMessage.value = e.message
                    rawEvents = emptyList()
                    events.value = emptyList()
                }
            } finally {
                if (gen == agendaGen) isLoading.value = false
            }
        }
    }

    fun loadInvitedEvents() {
        viewModelScope.launch {
            try {
                invitedEvents.value = api.queryMyEvents("invited")
            } catch (_: Exception) {
                invitedEvents.value = emptyList()
            }
        }
    }

    fun selectCalendar(calendar: FaunaCalendar) {
        selectedCalendar.value = calendar
        selectedEvent.value = null
        selectedEventAttendees.value = emptyList()
        // Narrow both surfaces to the selected calendar (agenda + grid). The
        // union stays reachable only from the no-selection state; selecting
        // scopes down (events.md § Layout and flow).
        refreshEvents()
        loadEventsForCurrentView()
    }

    fun selectEvent(event: EventSummary) {
        viewModelScope.launch {
            isLoading.value = true
            try {
                val detailDef = async { api.getEvent(event.id) }
                val attendeesDef = async { api.listEventAttendees(event.id) }
                selectedEvent.value = detailDef.await()
                selectedEventAttendees.value = attendeesDef.await()
                loadReminder(event.id)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
            isLoading.value = false
        }
    }

    fun createCalendar(name: String) {
        viewModelScope.launch {
            try {
                val before = calendars.value
                val beforeIds = before.map { it.id }.toSet()
                api.createCalendar(name)
                val after = api.listCalendars()
                calendars.value = after
                visibleCalendarIds.value = seedVisibleCalendarIds(visibleCalendarIds.value, before, after)
                // Target the just-created calendar for the next authoring action
                // (events.md § Layout and flow) — the create RPC returns no id, so
                // diff the list. The no-selection union stays visible; we do NOT
                // auto-select (matching the natives, which target-but-don't-select).
                lastCreatedCalendarId = after.firstOrNull { it.id !in beforeIds }?.id
                refreshEvents()
                loadEventsForCurrentView()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    fun createEvent(request: CreateEventRequest) {
        viewModelScope.launch {
            try {
                // In the no-selection union view the request carries an empty
                // calendarId (the screen has no selection) — target the active
                // (just-created / first available) calendar, mirroring apple's
                // `request.calendarId.isEmpty ? …` fill.
                val req = if (request.calendarId.isEmpty()) {
                    val cal = activeCalendar() ?: return@launch
                    request.copy(calendarId = cal.id)
                } else {
                    request
                }
                api.createEvent(req)
                // Rule 3: clear the rail on the SUCCESS path, not at the
                // submit click — a failed create must leave the draft intact
                // so the user's authored text is not lost to a transient error.
                draftsHost.clear()
                refreshEvents()
                loadEventsForCurrentView()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    fun deleteEvent() {
        viewModelScope.launch {
            val event = selectedEvent.value ?: return@launch
            try {
                api.deleteEvent(event.id)
                selectedEvent.value = null
                selectedEventAttendees.value = emptyList()
                // Re-query the current scope (selected calendar or the union) so
                // the deleted row drops — the delete itself is by event id.
                refreshEvents()
                loadEventsForCurrentView()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    /** Invite an attendee by **email** (the universal CalDAV/iMIP identifier on
     *  the encrypted path — the seam adds a `mailto:` ATTENDEE + fans out the
     *  iMIP REQUEST). */
    fun inviteAttendee(eventId: String, email: String) {
        viewModelScope.launch {
            try {
                api.inviteAttendee(eventId, email)
                selectedEventAttendees.value = api.listEventAttendees(eventId)
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    fun rsvp(eventId: String, response: RsvpResponse) {
        viewModelScope.launch {
            try {
                api.rsvpEvent(eventId, response)
                if (selectedEvent.value?.id == eventId) {
                    selectedEventAttendees.value = api.listEventAttendees(eventId)
                }
                loadInvitedEvents()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    fun loadReminder(eventId: String) {
        viewModelScope.launch {
            try {
                currentReminder.value = api.getReminder(eventId)
            } catch (_: Exception) {
                currentReminder.value = null
            }
        }
    }

    fun setReminder(eventId: String, offset: String) {
        viewModelScope.launch {
            try {
                api.setReminder(eventId, offset)
                currentReminder.value = offset
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    fun removeReminder(eventId: String) {
        viewModelScope.launch {
            try {
                api.removeReminder(eventId)
                currentReminder.value = null
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    fun importCalendar(calendarId: String, icsText: String) {
        viewModelScope.launch {
            try {
                api.importCalendar(calendarId, icsText)
                // Refresh the current scope (selected calendar or the union) so
                // the imported events appear.
                refreshEvents()
                loadEventsForCurrentView()
            } catch (e: Exception) {
                errorMessage.value = e.message
            }
        }
    }

    suspend fun exportCalendar(calendarId: String): String? {
        return try {
            api.exportCalendar(calendarId)
        } catch (e: Exception) {
            errorMessage.value = e.message
            null
        }
    }

    val calendarView = MutableStateFlow(FfiCalendarViewMode.AGENDA)
    val selectedDate = MutableStateFlow(LocalDate.now())
    val visibleEvents = MutableStateFlow<List<EventSummary>>(emptyList())

    fun selectView(view: FfiCalendarViewMode) {
        calendarView.value = view
        loadEventsForCurrentView()
    }

    fun selectDate(date: LocalDate) {
        selectedDate.value = date
        loadEventsForCurrentView()
    }

    /**
     * Pan the anchor by one visible range. [offset] carries only the
     * DIRECTION: how far is the shared policy's answer (`pannedBy`), not this
     * call site's, which is what stopped android holding the rule four times.
     */
    fun navigateDate(offset: Int) {
        selectedDate.value =
            selectedDate.value.pannedBy(calendarView.value, forward = offset >= 0)
        loadEventsForCurrentView()
    }

    private fun loadEventsForCurrentView() {
        val date = selectedDate.value
        val (start, end) = when (calendarView.value) {
            FfiCalendarViewMode.DAY -> date to date.plusDays(1)
            FfiCalendarViewMode.WEEK -> {
                val weekStart = date.snapToWeekStart(localeFirstDayOfWeek())
                weekStart to weekStart.plusDays(7)
            }
            FfiCalendarViewMode.MONTH -> {
                val ym = YearMonth.from(date)
                ym.atDay(1) to ym.plusMonths(1).atDay(1)
            }
            FfiCalendarViewMode.AGENDA -> return
        }
        // The grid shows the current scope: the selected calendar's events, or —
        // with none selected — the no-selection union of every owned calendar
        // (events.md § Implementation status, 2026-07-18). Previously bailed on no
        // selection, so android showed an empty grid until a calendar was picked.
        // The `rangeGen` guard drops a stale union result that resolves late.
        val gen = ++rangeGen
        viewModelScope.launch {
            try {
                val evs = scopedEventItems(selectedCalendar.value, calendars.value) {
                    api.queryEventsInRange(it, start.toString(), end.toString())
                }
                if (gen != rangeGen) return@launch
                rawVisibleEvents = evs
                visibleEvents.value = displayed(rawVisibleEvents)
            } catch (e: Exception) {
                if (gen == rangeGen) errorMessage.value = e.message
            }
        }
    }
}

/** The events for the current scope: the selected calendar's events, or — when
 *  none is selected, OR the selected calendar has since vanished (deleted here,
 *  or by a CalDAV MUA against the same `bridge_caldav_*` store) — the union of
 *  every owned calendar's events, deduped by id (the encrypted store has no
 *  cross-calendar query, so the client fans out). The live-vs-vanished check is
 *  the shared `fauna_client_caldav::resolve_calendar_selection` rule (events.md
 *  § Where logic lives → *"Which calendars the page is scoped to"*) so a
 *  deleted-out-from-under-you selection can never strand the page on a
 *  permanently blank list. Mirrors windows' `QueryEventItemsAsync`, apple's
 *  `EventsVM.queryEventItems`, and web's `queryEventItems` — the richest
 *  existing pattern converged onto per priority #4 (events.md §
 *  Implementation status — the no-selection union, 2026-07-18). Pure over an
 *  injected `query` so the branch + dedup are unit-testable without the
 *  ApiClient (EventsScopeTest). */
internal suspend fun scopedEventItems(
    selected: FaunaCalendar?,
    calendars: List<FaunaCalendar>,
    query: suspend (calendarId: String) -> List<EventSummary>,
): List<EventSummary> {
    val liveSelectedId = resolveCalendarSelection(selected?.id, calendars.map { it.id })
    if (liveSelectedId != null) return query(liveSelectedId)
    val byId = LinkedHashMap<String, EventSummary>()
    for (cal in calendars) {
        for (ev in query(cal.id)) byId[ev.id] = ev
    }
    return byId.values.toList()
}

/** `raw` filtered through the shared `calendar_is_displayed` composition
 *  (events.md § Where logic lives → *Which calendars display*): a live
 *  `selected` calendar wins outright (visibility never applies to a
 *  selection); with none, `visibleCalendarIds` filters the union, an
 *  **empty** set meaning "no filter" (the full union). An event with no
 *  `calendarId` is never hidden — defensive only, `toSummary()` always
 *  populates it. Pure over its inputs so it is unit-testable without the
 *  ApiClient (EventsScopeTest), mirroring [scopedEventItems]. */
internal fun displayedEvents(
    raw: List<EventSummary>,
    selected: FaunaCalendar?,
    calendars: List<FaunaCalendar>,
    visibleCalendarIds: Set<String>,
): List<EventSummary> {
    val existingIds = calendars.map { it.id }
    val visible = visibleCalendarIds.toList()
    return raw.filter { ev ->
        val calId = ev.calendarId ?: return@filter true
        calendarIsDisplayed(selected?.id, existingIds, visible, calId)
    }
}

/** [visible] filled on a fresh calendar list so the page paints every
 *  `calendar-visibility` box checked instead of leaning on the
 *  empty-set-is-union rule (mirrors tui/linux/web `seedVisibleCalendars`). A
 *  **brand-new** calendar (in `next` but not `previous`) starts visible; an
 *  existing one keeps whatever the user chose. Unchecking every box empties
 *  the set, which the shared predicate reads as the union — so the next load
 *  re-seeds it. */
internal fun seedVisibleCalendarIds(
    visible: Set<String>,
    previous: List<FaunaCalendar>,
    next: List<FaunaCalendar>,
): Set<String> {
    val previousIds = previous.map { it.id }.toSet()
    val seeded = visible.toMutableSet()
    for (cal in next) {
        if (cal.id !in previousIds) seeded.add(cal.id)
    }
    if (seeded.isEmpty()) {
        seeded.addAll(next.map { it.id })
    }
    return seeded
}
