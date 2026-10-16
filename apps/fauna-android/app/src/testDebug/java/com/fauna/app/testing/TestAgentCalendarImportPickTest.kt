package com.fauna.app.testing

import android.net.Uri
import androidx.compose.runtime.getValue
import androidx.compose.runtime.mutableStateOf
import androidx.compose.runtime.setValue
import androidx.compose.ui.test.junit4.createComposeRule
import androidx.compose.ui.test.onNodeWithTag
import androidx.compose.ui.test.performClick
import androidx.test.core.app.ApplicationProvider
import com.fauna.app.core.AppState
import com.fauna.app.core.SecureStorage
import com.fauna.app.data.api.FaunaCalendar
import com.fauna.app.ui.screen.events.EventsContent
import com.fauna.app.ui.screen.events.readIcsText
import com.fauna.ffi.FfiCalendarViewMode
import kotlinx.coroutines.runBlocking
import org.json.JSONObject
import org.junit.After
import org.junit.Assert.assertEquals
import org.junit.Assert.assertNotNull
import org.junit.Assert.assertNull
import org.junit.Assert.assertTrue
import org.junit.Rule
import org.junit.Test
import org.junit.rules.TemporaryFolder
import org.junit.runner.RunWith
import org.mockito.Mockito.mock
import org.robolectric.annotation.Config
import social.fauna.generated.Ids

/**
 * `compose.file`[calendar-import-file] — the harness door
 * `EventsActions.import_calendar_file` reaches android's Events import through
 * (`drivers/android.py::set_input_files` stages the host file on the device and
 * sends `{"file", "target"}`; `events.md` § Import / Export). android's
 * `calendar-import-file` is an `IconButton` that launches the OS `GetContent()`
 * picker, which no in-process agent can drive, so while the import row is shown
 * it registers the very staging its picker callback calls on
 * `TestAgent.calendarImportPick` — the Media page's shape
 * ([TestAgentMediaFilePickTest]).
 *
 * These pins compose the REAL [EventsContent] under a harness holding the
 * picked Uri exactly as `EventsScreen` does; `calendar-import-button` reads it
 * through the screen's own [readIcsText], so they assert what the import would
 * parse, never the arm's say-so.
 */
@RunWith(FaunaRobolectricTestRunner::class)
@Config(sdk = [34], qualifiers = "h1280dp")
class TestAgentCalendarImportPickTest {

    @get:Rule
    val compose = createComposeRule()

    @get:Rule
    val tmp = TemporaryFolder()

    private fun storage() = mock(SecureStorage::class.java)

    /** `messages.error` off the state protocol — what `error_text()` reads. */
    private fun errorFromStateProtocol(appState: AppState): String? {
        val state = TestAgent.serializeState(appState, storage(), null, appState.messages)
        if (state.isNull("messages")) return null
        val messages = state.getJSONObject("messages")
        return if (messages.isNull("error")) null else messages.getString("error")
    }

    /** The exact command `set_input_files("calendar-import-file", path)` sends. */
    private fun filePatch(path: String, appState: AppState) = runBlocking {
        val cmd = JSONObject().apply {
            put("action", "patch")
            put(
                "state",
                JSONObject().put(
                    "compose",
                    JSONObject().put("file", path).put("target", "calendar-import-file"),
                ),
            )
        }
        TestAgent.processCommand("patch", cmd, appState, storage(), null)
    }

    private var shown by mutableStateOf(true)
    private var picked by mutableStateOf<Uri?>(null)

    /** What `calendar-import-button` read from the pick. */
    private val imported = mutableListOf<String>()

    /** Compose the page the way `EventsScreen` wires it: the pick lands in
     *  screen-held state, `calendar-import-button` reads it via [readIcsText]. */
    private fun openPage(selected: FaunaCalendar? = FaunaCalendar(id = "cal-1", name = "Work")) {
        val resolver = ApplicationProvider.getApplicationContext<android.content.Context>().contentResolver
        compose.setContent {
            if (shown) {
                EventsContent(
                    calendars = listOfNotNull(selected),
                    selectedCalendar = selected,
                    visibleCalendarIds = setOf("cal-1"),
                    events = emptyList(),
                    invitedEvents = emptyList(),
                    visibleEvents = emptyList(),
                    isLoading = false,
                    calendarView = FfiCalendarViewMode.AGENDA,
                    selectedDate = java.time.LocalDate.of(2026, 9, 1),
                    onRefresh = {},
                    onSelectCalendar = {},
                    onToggleCalendarVisibility = {},
                    onSelectView = {},
                    onSelectDate = {},
                    onOpenEvent = {},
                    onImportIcs = { picked?.let { uri -> readIcsText(resolver, uri)?.let { imported += it } } },
                    onExportIcs = {},
                    pickedIcsName = picked?.lastPathSegment,
                    onIcsPicked = { picked = it },
                    onRsvp = { _, _ -> },
                    onCreateCalendar = {},
                    onCreateEvent = {},
                )
            }
        }
        compose.waitForIdle()
    }

    private fun staged(name: String, text: String): String =
        tmp.newFile(name).apply { writeText(text) }.absolutePath

    @After
    fun unregister() {
        // `TestAgent` is a process-global `object` — never leak a handler.
        TestAgent.calendarImportPick = null
    }

    /** The load-bearing case: the staged file becomes the page's pick, and
     *  `calendar-import-button` reads its own text. */
    @Test
    fun theStagedFileReachesTheImportThroughImportButton() {
        openPage()
        assertNotNull("the import row registers its pick handler", TestAgent.calendarImportPick)
        val appState = AppState()

        filePatch(staged("work.ics", "BEGIN:VCALENDAR\nEND:VCALENDAR"), appState)
        compose.waitForIdle()

        assertNull("an honoured pick writes no refusal", errorFromStateProtocol(appState))
        compose.onNodeWithTag(Ids.CALENDAR_IMPORT_BUTTON).performClick()
        compose.waitForIdle()
        assertEquals(listOf("BEGIN:VCALENDAR\nEND:VCALENDAR"), imported)
    }

    /** No Events page open → a named refusal, never a silent no-op
     *  (convention 11). */
    @Test
    fun aPickWithNoPageOpenIsRefusedByName() {
        val appState = AppState()

        filePatch(staged("work.ics", "x"), appState)

        val shownError = errorFromStateProtocol(appState)
        assertNotNull("a pick with no page open must be loud", shownError)
        assertTrue("the reason must say the row is not open: $shownError", shownError!!.contains("not open"))
    }

    /** No calendar selected removes the import row, so it holds no handler. */
    @Test
    fun noSelectedCalendarHasNoImportRowToPickInto() {
        openPage(selected = null)
        val appState = AppState()

        filePatch(staged("work.ics", "x"), appState)

        assertNull(TestAgent.calendarImportPick)
        assertNotNull(errorFromStateProtocol(appState))
        assertNull("nothing staged", picked)
    }

    /** Leaving the page unregisters the handler. */
    @Test
    fun thePageUnregistersWhenItCloses() {
        openPage()
        shown = false
        compose.waitForIdle()

        assertNull(TestAgent.calendarImportPick)
    }

    /** A missing staged file is refused by name, not handed to the page. */
    @Test
    fun aMissingStagedFileIsRefusedByName() {
        openPage()
        val appState = AppState()

        filePatch(tmp.root.resolve("absent.ics").absolutePath, appState)

        assertNotNull(errorFromStateProtocol(appState))
        assertNull("nothing staged", picked)
    }

    /** A pick the provider cannot open reads as null — the screen then says so
     *  on `error-message` rather than dropping the import (convention 11). */
    @Test
    fun anUnreadablePickReadsAsNull() {
        val resolver = ApplicationProvider.getApplicationContext<android.content.Context>().contentResolver
        val gone = Uri.fromFile(tmp.root.resolve("gone.ics"))

        assertNull(readIcsText(resolver, gone))
    }
}
