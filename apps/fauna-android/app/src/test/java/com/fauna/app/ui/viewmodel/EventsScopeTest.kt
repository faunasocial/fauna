package com.fauna.app.ui.viewmodel

import com.fauna.app.data.api.EventSummary
import com.fauna.app.data.api.FaunaCalendar
import com.fauna.app.testing.FaunaRobolectricTestRunner
import kotlinx.coroutines.test.runTest
import org.junit.Assert.assertEquals
import org.junit.Test
import org.junit.runner.RunWith

/**
 * Unit tests for [scopedEventItems], the pure fan-out + dedup behind the Events
 * **no-selection union** (events.md § Implementation status — the union converge,
 * 2026-07-18) and the **live-vs-vanished selection** fallback (events.md § Where
 * logic lives → *"Which calendars the page is scoped to"*, 2026-07-30). Contract:
 * with a LIVE calendar selected the view is only that calendar's events; with
 * none selected, OR a selection whose calendar has since vanished, it is the
 * union of every owned calendar's events, deduped by id (the encrypted store has
 * no cross-calendar query, so the client fans out). This converges android onto
 * windows' `QueryEventItemsAsync`, apple's `EventsVM.queryEventItems`, and web's
 * `queryEventItems` (priority #4) — android was the one app that showed
 * nothing until a calendar was picked, and (until the vanished-selection fix)
 * the one app that kept querying a deleted calendar forever.
 *
 * `resolveCalendarSelection` is a real UniFFI/JNA call into
 * `fauna_client_caldav::resolve_calendar_selection`, hence `FaunaRobolectricTestRunner`
 * (the shared classloader that keeps one UniFFI handle map — testing.md § tier_1
 * on android) rather than plain JUnit.
 */
@RunWith(FaunaRobolectricTestRunner::class)
class EventsScopeTest {

    private fun cal(id: String) =
        FaunaCalendar(id = id, name = "cal-$id")

    private fun ev(id: String, calendarId: String? = null) =
        EventSummary(
            id = id, uid = "u-$id", summary = "e-$id", dtstart = "2026-07-18T10:00:00",
            calendarId = calendarId,
        )

    @Test
    fun selectedCalendar_returnsOnlyThatCalendarsEvents() = runTest {
        val fannedOut = mutableListOf<String>()
        val result = scopedEventItems(
            selected = cal("work"),
            calendars = listOf(cal("work"), cal("home")),
        ) { id ->
            fannedOut.add(id)
            if (id == "work") listOf(ev("a"), ev("b")) else listOf(ev("c"))
        }

        assertEquals(listOf("a", "b"), result.map { it.id })
        // Fans out to ONLY the selected calendar — home is never queried.
        assertEquals(listOf("work"), fannedOut)
    }

    @Test
    fun vanishedSelection_fallsBackToUnion() = runTest {
        // "work" was selected, but has since been deleted (here, or by an
        // external CalDAV MUA) — the page must not keep querying it forever.
        val fannedOut = mutableListOf<String>()
        val result = scopedEventItems(
            selected = cal("work"),
            calendars = listOf(cal("home")),
        ) { id ->
            fannedOut.add(id)
            if (id == "work") listOf(ev("a")) else listOf(ev("c"))
        }

        assertEquals(setOf("c"), result.map { it.id }.toSet())
        assertEquals(listOf("home"), fannedOut)
    }

    @Test
    fun noSelection_unionsEveryOwnedCalendar() = runTest {
        val result = scopedEventItems(
            selected = null,
            calendars = listOf(cal("work"), cal("home")),
        ) { id -> if (id == "work") listOf(ev("a"), ev("b")) else listOf(ev("c")) }

        assertEquals(setOf("a", "b", "c"), result.map { it.id }.toSet())
    }

    @Test
    fun noSelection_dedupesById() = runTest {
        // The same event id surfacing from two calendars collapses to one row
        // (later occurrence wins, matching web/apple's `byId` upsert).
        val result = scopedEventItems(
            selected = null,
            calendars = listOf(cal("work"), cal("home")),
        ) { id ->
            if (id == "work") listOf(ev("dup"), ev("a")) else listOf(ev("dup"), ev("c"))
        }

        assertEquals(listOf("dup", "a", "c"), result.map { it.id })
        assertEquals(3, result.size)
    }

    @Test
    fun noSelection_noCalendars_isEmpty() = runTest {
        val result = scopedEventItems(selected = null, calendars = emptyList()) { emptyList() }
        assertEquals(emptyList<String>(), result.map { it.id })
    }
}

/**
 * Unit tests for [displayedEvents] and [seedVisibleCalendarIds], the pure
 * functions behind android's `calendar-visibility` display filter (events.md
 * § Where logic lives → *Which calendars display*, ratified 2026-08-02).
 * `calendarIsDisplayed` is a real UniFFI/JNA call, hence
 * `FaunaRobolectricTestRunner` like [EventsScopeTest].
 */
@RunWith(FaunaRobolectricTestRunner::class)
class EventsVisibilityFilterTest {

    private fun cal(id: String) = FaunaCalendar(id = id, name = "cal-$id")

    private fun ev(id: String, calendarId: String) =
        EventSummary(
            id = id, uid = "u-$id", summary = "e-$id", dtstart = "2026-07-18T10:00:00",
            calendarId = calendarId,
        )

    @Test
    fun emptyVisibleSet_isTheFullUnion() {
        val raw = listOf(ev("a", "work"), ev("b", "home"))
        val result = displayedEvents(raw, selected = null, calendars = listOf(cal("work"), cal("home")), visibleCalendarIds = emptySet())
        assertEquals(setOf("a", "b"), result.map { it.id }.toSet())
    }

    @Test
    fun hiddenCalendar_dropsOnlyItsEvents() {
        val raw = listOf(ev("a", "work"), ev("b", "home"))
        // "work" unchecked: only "home" is in the visible set.
        val result = displayedEvents(raw, selected = null, calendars = listOf(cal("work"), cal("home")), visibleCalendarIds = setOf("home"))
        assertEquals(listOf("b"), result.map { it.id })
    }

    @Test
    fun reCheckingRestoresTheEvent() {
        val raw = listOf(ev("a", "work"), ev("b", "home"))
        val result = displayedEvents(raw, selected = null, calendars = listOf(cal("work"), cal("home")), visibleCalendarIds = setOf("work", "home"))
        assertEquals(setOf("a", "b"), result.map { it.id }.toSet())
    }

    @Test
    fun liveSelection_winsOutrightOverVisibility() {
        // "work" is selected AND unchecked — the selection still wins; visibility
        // never applies to a selection (events.md § Where logic lives).
        val raw = listOf(ev("a", "work"))
        val result = displayedEvents(raw, selected = cal("work"), calendars = listOf(cal("work"), cal("home")), visibleCalendarIds = emptySet())
        assertEquals(listOf("a"), result.map { it.id })
    }

    @Test
    fun vanishedSelection_fallsBackToVisibilityFilteredUnion() {
        // "work" was selected but has since vanished — falls back to the union,
        // same staleness rule scopedEventItems uses, still gated by visibility.
        val raw = listOf(ev("a", "work"), ev("b", "home"))
        val result = displayedEvents(raw, selected = cal("work"), calendars = listOf(cal("home")), visibleCalendarIds = setOf("home"))
        assertEquals(listOf("b"), result.map { it.id })
    }

    @Test
    fun seed_freshCalendarStartsVisible() {
        val seeded = seedVisibleCalendarIds(visible = emptySet(), previous = emptyList(), next = listOf(cal("work")))
        assertEquals(setOf("work"), seeded)
    }

    @Test
    fun seed_existingCalendarKeepsUserChoice() {
        // "work" was manually unchecked; a reload with no new calendars must not
        // re-check it.
        val seeded = seedVisibleCalendarIds(
            visible = setOf("home"),
            previous = listOf(cal("work"), cal("home")),
            next = listOf(cal("work"), cal("home")),
        )
        assertEquals(setOf("home"), seeded)
    }

    @Test
    fun seed_brandNewCalendarStartsVisibleAlongsideExistingChoices() {
        val seeded = seedVisibleCalendarIds(
            visible = setOf("home"),
            previous = listOf(cal("home")),
            next = listOf(cal("home"), cal("new")),
        )
        assertEquals(setOf("home", "new"), seeded)
    }

    @Test
    fun seed_allUncheckedThenReload_reincludesEverything() {
        // Unchecking every box empties the set (the union, per the predicate);
        // the NEXT load must not treat that as "nothing new" and leave it empty.
        val seeded = seedVisibleCalendarIds(
            visible = emptySet(),
            previous = listOf(cal("work"), cal("home")),
            next = listOf(cal("work"), cal("home")),
        )
        assertEquals(setOf("work", "home"), seeded)
    }
}
