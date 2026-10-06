package com.fauna.app.ui.screen.events

import android.content.Intent
import androidx.activity.compose.rememberLauncherForActivityResult
import androidx.activity.result.contract.ActivityResultContracts
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.LazyRow
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.material3.pulltorefresh.PullToRefreshContainer
import androidx.compose.material3.pulltorefresh.rememberPullToRefreshState
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.input.nestedscroll.nestedScroll
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.ffi.FfiCalendarViewMode
import com.fauna.ffi.FfiEventDrafts
import com.fauna.ffi.calendarViewModes
import uniffi.fauna_core.RsvpResponse
import com.fauna.app.data.api.CreateEventRequest
import com.fauna.app.data.api.EventSummary
import com.fauna.app.data.api.FaunaCalendar
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.format.DateTimeFormatter
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.viewmodel.EventsVM
import kotlinx.coroutines.launch
import social.fauna.generated.Ids

/**
 * The datetime shape the create-event form's own fields take (their placeholder
 * is `YYYY-MM-DDTHH:MM`); `normalizeEventDatetimeInput` pads it to seconds on
 * submit, so a prefill and a hand-typed value travel the identical path.
 */
private val FORM_DATETIME = DateTimeFormatter.ofPattern("yyyy-MM-dd'T'HH:mm")

/**
 * The VM-bound `events` page the NavHost mounts. Everything renderable lives in
 * the stateless [EventsContent] so the Compose test harness can drive the page's
 * controls without a ViewModel; this wrapper keeps only what genuinely needs the
 * Activity — the `.ics` file picker and the export share-sheet intent.
 */
@Composable
fun EventsScreen(navController: NavController, vm: EventsVM = hiltViewModel()) {
    val calendars by vm.calendars.collectAsState()
    val selectedCalendar by vm.selectedCalendar.collectAsState()
    val visibleCalendarIds by vm.visibleCalendarIds.collectAsState()
    val events by vm.events.collectAsState()
    val invitedEvents by vm.invitedEvents.collectAsState()
    val isLoading by vm.isLoading.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val calendarView by vm.calendarView.collectAsState()
    val selectedDate by vm.selectedDate.collectAsState()
    val visibleEvents by vm.visibleEvents.collectAsState()
    val draft = vm.draft.collectAsState()
    val appMessages = LocalAppMessages.current

    val context = LocalContext.current
    val scope = rememberCoroutineScope()

    val icsPickerLauncher = rememberLauncherForActivityResult(
        ActivityResultContracts.GetContent()
    ) { uri ->
        uri ?: return@rememberLauncherForActivityResult
        val calId = selectedCalendar?.id ?: return@rememberLauncherForActivityResult
        scope.launch {
            val icsText = context.contentResolver.openInputStream(uri)?.bufferedReader()?.readText()
            if (icsText != null) {
                vm.importCalendar(calId, icsText)
            }
        }
    }

    LaunchedEffect(Unit) {
        // Render the no-selection union on entry (every owned calendar's events)
        // — android showed nothing until a calendar was picked before the union
        // lift (events.md § Implementation status, 2026-07-18).
        vm.load()
    }

    LaunchedEffect(errorMessage) {
        appMessages.showError(errorMessage)
    }

    EventsContent(
        calendars = calendars,
        selectedCalendar = selectedCalendar,
        visibleCalendarIds = visibleCalendarIds,
        events = events,
        invitedEvents = invitedEvents,
        visibleEvents = visibleEvents,
        isLoading = isLoading,
        calendarView = calendarView,
        selectedDate = selectedDate,
        onRefresh = { vm.load() },
        onSelectCalendar = vm::selectCalendar,
        onToggleCalendarVisibility = vm::toggleCalendarVisibility,
        onSelectView = vm::selectView,
        onSelectDate = vm::selectDate,
        onOpenEvent = { id -> navController.navigate("event/$id") },
        onImportIcs = { icsPickerLauncher.launch("text/calendar") },
        onExportIcs = {
            scope.launch {
                val calId = selectedCalendar?.id ?: return@launch
                val icsText = vm.exportCalendar(calId)
                if (icsText != null) {
                    val intent = Intent(Intent.ACTION_SEND).apply {
                        type = "text/calendar"
                        putExtra(Intent.EXTRA_TEXT, icsText)
                    }
                    context.startActivity(Intent.createChooser(intent, "Export Calendar"))
                }
            }
        },
        onRsvp = vm::rsvp,
        onCreateCalendar = vm::createCalendar,
        onCreateEvent = vm::createEvent,
        draft = draft,
        onDraftEdited = vm::onDraftEdited,
        onStartFreshDraft = vm::clearDraft,
    )
}

/**
 * The whole `events` surface, stateless.
 *
 * ## What declares a wire kind here
 *
 * Four controls issue a class-3 (OnlineOnly) kind: the three invited-event RSVP
 * buttons and the `.ics` **import** — all `fauna.bridges.put_event_ciphertext`,
 * since an RSVP re-PUTs the VEVENT after applying PARTSTAT and an import parses,
 * seals and PUTs every VEVENT it reads
 * (`fauna_ffi::caldav_client::import_calendar_ics`). The two dialogs carry the
 * remaining two: [CreateCalendarDialog]'s confirm
 * (`fauna.bridges.provision_calendar`) and [EventFormSheet]'s submit (the same
 * VEVENT-writer kind).
 *
 * ⚠ The `.ics` import is **beyond the lead app's gesture set** — tui has no
 * import control, so the rule-4 oracle differential will never flag it. It was
 * found by following android's own dispatcher to its `request(...)`, the method
 * this row's batch 10 recorded after four "unbuilt section" verdicts turned out
 * to be renamed controls.
 *
 * Its **export** sibling is a pure read (`query_decoded` + `generate_ical_multi`)
 * and declares nothing — ruling 1: a read is never greyed on its own account.
 * Neither does the navigating half: view mode, range panning, the month→day
 * drill-in, both compose openers, the calendar chips and their visibility
 * checkboxes. Greying those would strand a viewer on whatever range they
 * happened to be looking at when the connection dropped, with a calendar they
 * can still read.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EventsContent(
    calendars: List<FaunaCalendar>,
    selectedCalendar: FaunaCalendar?,
    visibleCalendarIds: Set<String>,
    events: List<EventSummary>,
    invitedEvents: List<EventSummary>,
    visibleEvents: List<EventSummary>,
    isLoading: Boolean,
    calendarView: FfiCalendarViewMode,
    selectedDate: LocalDate,
    onRefresh: () -> Unit,
    onSelectCalendar: (FaunaCalendar) -> Unit,
    onToggleCalendarVisibility: (String) -> Unit,
    onSelectView: (FfiCalendarViewMode) -> Unit,
    onSelectDate: (LocalDate) -> Unit,
    onOpenEvent: (String) -> Unit,
    onImportIcs: () -> Unit,
    onExportIcs: () -> Unit,
    onRsvp: (eventId: String, response: RsvpResponse) -> Unit,
    onCreateCalendar: (name: String) -> Unit,
    onCreateEvent: (CreateEventRequest) -> Unit,
    // Draft-persistence v2, events rail (reserved-folders.md § Drafts Sync;
    // events.md § Persistence). `draft` is reactive (not a one-shot read) so a
    // restore landing while the New Event sheet is already open still reaches
    // it — the late-restore rule. `onDraftEdited` fires on every form
    // keystroke (never on mount — see EventFormSheet); `onStartFreshDraft`
    // clears the rail for a day-cell "start fresh" open (rule 2) and for a
    // successful create (rule 3, called on the success path in the VM).
    draft: State<FfiEventDrafts?> = remember { mutableStateOf(null) },
    onDraftEdited: (
        summary: String, dtstart: String, dtend: String, description: String, location: String,
    ) -> Unit = { _, _, _, _, _ -> },
    onStartFreshDraft: () -> Unit = {},
) {
    var showCreateCalendar by remember { mutableStateOf(false) }
    var showEventForm by remember { mutableStateOf(false) }
    // The compose prefill an empty-slot click seeds; null for the FAB's blank
    // form. Single-sourced here so the two entry points cannot drift (windows'
    // `NewEventAtSlot` plays the same role).
    var newEventSlot by remember { mutableStateOf<LocalDateTime?>(null) }

    val pullToRefreshState = rememberPullToRefreshState()
    if (pullToRefreshState.isRefreshing) {
        LaunchedEffect(true) {
            // Re-list calendars + invited events and re-render the current scope
            // (selected calendar or the no-selection union).
            onRefresh()
            pullToRefreshState.endRefresh()
        }
    }

    Scaffold(
        floatingActionButton = {
            // Authoring is available from the no-selection union view too: the
            // event targets the active (just-created / first available) calendar
            // (events.md § Layout and flow). Needs at least one calendar to exist.
            // A compose OPENER writes nothing, so it stays live with no nest —
            // the form's own submit is where the gate lands.
            if (calendars.isNotEmpty()) {
                FloatingActionButton(
                    onClick = { newEventSlot = null; showEventForm = true },
                    modifier = Modifier.testTag(Ids.NEW_EVENT_BTN)
                ) {
                    Icon(Icons.Default.Add, stringResource(R.string.events_new_event))
                }
            }
        }
    ) { padding ->
        Box(
            modifier = Modifier
                .padding(padding)
                .fillMaxSize()
                .nestedScroll(pullToRefreshState.nestedScrollConnection)
        ) {
            LazyColumn(
                modifier = Modifier.fillMaxSize(),
                contentPadding = PaddingValues(vertical = 8.dp)
            ) {
                // Calendar picker
                item {
                    LazyRow(
                        contentPadding = PaddingValues(horizontal = 16.dp),
                        horizontalArrangement = Arrangement.spacedBy(8.dp)
                    ) {
                        items(calendars) { cal ->
                            // Two affordances per calendar (events.md § Where logic
                            // lives → *Which calendars display*, ratified 2026-08-02):
                            // the chip selects (narrows the agenda/grid to just this
                            // calendar), the checkbox toggles its display-filter
                            // membership in the no-selection union — independent
                            // concepts, never one control. Both are local view
                            // state and issue nothing.
                            Column(
                                horizontalAlignment = Alignment.CenterHorizontally,
                            ) {
                                FilterChip(
                                    selected = selectedCalendar?.id == cal.id,
                                    onClick = { onSelectCalendar(cal) },
                                    label = { Text(cal.name) },
                                    modifier = Modifier.testTag(Ids.CALENDAR_ITEM),
                                    leadingIcon = cal.color?.let { colorStr ->
                                        {
                                            Box(
                                                modifier = Modifier
                                                    .size(8.dp)
                                                    .clip(CircleShape)
                                                    .background(parseCalendarColor(colorStr))
                                            )
                                        }
                                    }
                                )
                                Checkbox(
                                    checked = visibleCalendarIds.contains(cal.id),
                                    onCheckedChange = { onToggleCalendarVisibility(cal.id) },
                                    modifier = Modifier.testTag(Ids.CALENDAR_VISIBILITY),
                                )
                            }
                        }
                        item {
                            IconButton(
                                onClick = { showCreateCalendar = true },
                                modifier = Modifier.testTag(Ids.NEW_CALENDAR_BTN)
                            ) {
                                Icon(Icons.Default.Add, stringResource(R.string.events_new_calendar))
                            }
                        }
                    }
                }

                // View selector
                item {
                    SingleChoiceSegmentedButtonRow(
                        modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 4.dp).testTag(Ids.EVENTS_VIEW_TOGGLE)
                    ) {
                        // Order and ids both come from the shared vocabulary:
                        // `calendarViewModes()` is `CalendarViewMode::ALL` and the
                        // tag is `calendar-view-` + the shared wire word, so this
                        // switcher cannot drift from the other six apps' order or
                        // spelling the way the hand-written `when` did.
                        val modes = calendarViewModes()
                        modes.forEachIndexed { index, view ->
                            val viewTag = calendarViewTag(view)
                            SegmentedButton(
                                selected = calendarView == view,
                                onClick = { onSelectView(view) },
                                shape = SegmentedButtonDefaults.itemShape(
                                    index = index, count = modes.size
                                ),
                                modifier = Modifier.testTag(viewTag)
                            ) { Text(view.name.lowercase().replaceFirstChar { it.uppercase() }) }
                        }
                    }
                }

                // Import/Export buttons
                if (selectedCalendar != null) {
                    item {
                        Column(modifier = Modifier.padding(horizontal = 16.dp)) {
                            // Import PUTs every VEVENT it parses; export only
                            // reads. So they sit side by side with opposite
                            // verdicts, which is the pairing the gate test reads.
                            val importGate = faunaGate("fauna.bridges.put_event_ciphertext")
                            Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                TextButton(onClick = onImportIcs, enabled = importGate.enabled) {
                                    Text(stringResource(R.string.events_import_ics))
                                }
                                TextButton(onClick = onExportIcs) {
                                    Text(stringResource(R.string.events_export_ics))
                                }
                            }
                            DisabledControlReasonText(importGate.reason)
                        }
                    }
                }

                // Invited events section
                if (invitedEvents.isNotEmpty()) {
                    item {
                        Text(
                            stringResource(R.string.events_invited_events),
                            style = MaterialTheme.typography.titleSmall,
                            modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp)
                        )
                    }
                    items(invitedEvents) { event ->
                        Card(
                            modifier = Modifier
                                .fillMaxWidth()
                                .padding(horizontal = 16.dp, vertical = 4.dp)
                        ) {
                            Column(modifier = Modifier.padding(16.dp)) {
                                Text(event.summary, style = MaterialTheme.typography.titleSmall)
                                Text(event.dtstart, style = MaterialTheme.typography.bodySmall,
                                    color = MaterialTheme.colorScheme.onSurfaceVariant)
                                Spacer(Modifier.height(8.dp))
                                // Every RSVP re-PUTs the VEVENT after applying
                                // PARTSTAT locally, so all three declare the
                                // VEVENT-writer kind — one verdict, since they
                                // issue one kind.
                                val rsvpGate = faunaGate("fauna.bridges.put_event_ciphertext")
                                Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
                                    Button(
                                        onClick = { onRsvp(event.id, RsvpResponse.GOING) },
                                        enabled = rsvpGate.enabled,
                                        modifier = Modifier.testTag(Ids.EVENT_RSVP_GOING)
                                    ) {
                                        Text(stringResource(R.string.events_rsvp_going))
                                    }
                                    OutlinedButton(
                                        onClick = { onRsvp(event.id, RsvpResponse.INTERESTED) },
                                        enabled = rsvpGate.enabled,
                                        modifier = Modifier.testTag(Ids.EVENT_RSVP_INTERESTED)
                                    ) {
                                        Text(stringResource(R.string.events_rsvp_interested))
                                    }
                                    OutlinedButton(
                                        onClick = { onRsvp(event.id, RsvpResponse.DECLINED) },
                                        enabled = rsvpGate.enabled,
                                        modifier = Modifier.testTag(Ids.EVENT_RSVP_DECLINE)
                                    ) {
                                        Text(stringResource(R.string.common_decline))
                                    }
                                }
                                DisabledControlReasonText(rsvpGate.reason)
                            }
                        }
                    }
                }

                // Content based on view. Renders the current scope: the selected
                // calendar's events, or — with none selected — the no-selection
                // union of every owned calendar (the VM feeds `events` /
                // `visibleEvents` accordingly). Before the union lift this was
                // gated on `selectedCalendar != null` and showed a "Select a
                // calendar" placeholder (events.md § Implementation status).
                if (calendars.isNotEmpty()) {
                    when (calendarView) {
                        FfiCalendarViewMode.AGENDA -> {
                            if (isLoading && events.isEmpty()) {
                                item {
                                    Box(modifier = Modifier.fillMaxWidth().padding(32.dp),
                                        contentAlignment = Alignment.Center) { CircularProgressIndicator() }
                                }
                            } else if (!isLoading && events.isEmpty()) {
                                item {
                                    Box(modifier = Modifier.fillMaxWidth().padding(32.dp),
                                        contentAlignment = Alignment.Center) {
                                        Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                            Icon(Icons.Default.DateRange, contentDescription = null,
                                                modifier = Modifier.size(48.dp),
                                                tint = MaterialTheme.colorScheme.onSurfaceVariant)
                                            Spacer(Modifier.height(8.dp))
                                            Text(stringResource(R.string.events_no_events_yet), color = MaterialTheme.colorScheme.onSurfaceVariant)
                                        }
                                    }
                                }
                            } else {
                                item {
                                    Text(selectedCalendar?.name ?: stringResource(R.string.events_title),
                                        style = MaterialTheme.typography.titleSmall,
                                        modifier = Modifier.padding(horizontal = 16.dp, vertical = 8.dp))
                                }
                                items(events) { event ->
                                    ListItem(
                                        headlineContent = { Text(event.summary, modifier = Modifier.testTag(Ids.EVENT_CARD_SUMMARY)) },
                                        supportingContent = { Text(event.dtstart) },
                                        modifier = Modifier.testTag(Ids.EVENT_CARD).padding(horizontal = 8.dp)
                                            .clickable { onOpenEvent(event.id) },
                                        colors = ListItemDefaults.colors(
                                            containerColor = MaterialTheme.colorScheme.surface))
                                    HorizontalDivider(modifier = Modifier.padding(horizontal = 16.dp))
                                }
                            }
                        }
                        FfiCalendarViewMode.DAY -> {
                            item {
                                DayTimelineContent(
                                    date = selectedDate,
                                    events = visibleEvents,
                                    onDateChange = onSelectDate,
                                    onEventClick = { onOpenEvent(it.id) },
                                    onSlotClick = { newEventSlot = it; onStartFreshDraft(); showEventForm = true })
                            }
                        }
                        FfiCalendarViewMode.WEEK -> {
                            item {
                                WeekGridContent(
                                    date = selectedDate,
                                    events = visibleEvents,
                                    onDateChange = onSelectDate,
                                    onEventClick = { onOpenEvent(it.id) },
                                    onSlotClick = { newEventSlot = it; onStartFreshDraft(); showEventForm = true })
                            }
                        }
                        FfiCalendarViewMode.MONTH -> {
                            item {
                                MonthGridContent(
                                    date = selectedDate,
                                    events = visibleEvents,
                                    onDateChange = onSelectDate,
                                    onDayClick = {
                                        onSelectDate(it)
                                        onSelectView(FfiCalendarViewMode.DAY)
                                    })
                            }
                        }
                    }
                } else if (!isLoading) {
                    item {
                        Box(modifier = Modifier.fillMaxWidth().padding(32.dp),
                            contentAlignment = Alignment.Center) {
                            Column(horizontalAlignment = Alignment.CenterHorizontally) {
                                Icon(Icons.Default.DateRange, contentDescription = null,
                                    modifier = Modifier.size(48.dp),
                                    tint = MaterialTheme.colorScheme.onSurfaceVariant)
                                Spacer(Modifier.height(8.dp))
                                Text(stringResource(R.string.events_no_calendars), color = MaterialTheme.colorScheme.onSurfaceVariant)
                                Spacer(Modifier.height(8.dp))
                                Button(onClick = { showCreateCalendar = true }) { Text(stringResource(R.string.events_new_calendar)) }
                            }
                        }
                    }
                }
            }

            PullToRefreshContainer(
                state = pullToRefreshState,
                modifier = Modifier.align(Alignment.TopCenter)
            )
        }
    }

    // Create Calendar Dialog
    if (showCreateCalendar) {
        CreateCalendarDialog(
            onDismiss = { showCreateCalendar = false },
            onCreate = { name ->
                onCreateCalendar(name)
                showCreateCalendar = false
            }
        )
    }

    // Event Form Sheet
    if (showEventForm) {
        EventFormSheet(
            calendarId = selectedCalendar?.id ?: "",
            // An empty-slot click prefills the column's date AND the slot's time,
            // one hour long (events.md § Week & day timeline views); the FAB
            // leaves both blank.
            initialDtstart = newEventSlot?.format(FORM_DATETIME).orEmpty(),
            initialDtend = newEventSlot?.plusHours(1)?.format(FORM_DATETIME).orEmpty(),
            // A day-cell open never resumes (rule 2 — onStartFreshDraft already
            // cleared the rail above), only the FAB's blank-slate open does.
            resumableDraft = if (newEventSlot == null) draft else null,
            onFieldsChanged = onDraftEdited,
            onDismiss = { showEventForm = false },
            onCreate = { request ->
                onCreateEvent(request)
                showEventForm = false
            }
        )
    }
}

private fun parseCalendarColor(color: String): Color {
    return try {
        Color(android.graphics.Color.parseColor(
            if (color.startsWith("#")) color else "#$color"
        ))
    } catch (_: Exception) {
        Color.Gray
    }
}

@Composable
fun CreateCalendarDialog(
    onDismiss: () -> Unit,
    onCreate: (name: String) -> Unit
) {
    var name by remember { mutableStateOf("") }

    AlertDialog(
        onDismissRequest = onDismiss,
        title = { Text(stringResource(R.string.events_new_calendar)) },
        text = {
            Column {
                OutlinedTextField(
                    value = name,
                    onValueChange = { name = it },
                    label = { Text(stringResource(R.string.events_calendar_name)) },
                    singleLine = true,
                    modifier = Modifier.fillMaxWidth().testTag(Ids.CALENDAR_NAME)
                )
            }
        },
        confirmButton = {
            // The page's own predicate (a name was typed) is handed to the gate
            // so this call site is the only author of `enabled =`; the gate never
            // enables what the form disabled.
            val gate = faunaGate("fauna.bridges.provision_calendar", enabled = name.isNotBlank())
            Column {
                TextButton(
                    onClick = { onCreate(name) },
                    enabled = gate.enabled,
                    modifier = Modifier.testTag(Ids.CREATE_CALENDAR)
                ) {
                    Text(stringResource(R.string.common_create))
                }
                DisabledControlReasonText(gate.reason)
            }
        },
        dismissButton = {
            TextButton(onClick = onDismiss) { Text(stringResource(R.string.common_cancel)) }
        }
    )
}

@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EventFormSheet(
    calendarId: String,
    onDismiss: () -> Unit,
    onCreate: (CreateEventRequest) -> Unit,
    initialDtstart: String = "",
    initialDtend: String = "",
    // Draft-persistence v2, events rail. `null` (a day-cell fresh start) means
    // never resume — the sheet stays on `initialDtstart`/`initialDtend` only.
    // Non-null (the FAB's blank-slate open) resumes the moment a non-empty
    // draft is observed AND the form still holds nothing user-authored — which
    // covers the ordinary case (already loaded at open) and the late-restore
    // case (the launch load lands after the sheet is already up) in the one
    // check, since both look identical from here: an unconsumed non-null draft
    // arriving while the fields are still blank.
    resumableDraft: State<FfiEventDrafts?>? = null,
    onFieldsChanged: (
        summary: String, dtstart: String, dtend: String, description: String, location: String,
    ) -> Unit = { _, _, _, _, _ -> },
) {
    var summary by remember { mutableStateOf("") }
    var dtstart by remember { mutableStateOf(initialDtstart) }
    var dtend by remember { mutableStateOf(initialDtend) }
    var description by remember { mutableStateOf("") }
    var location by remember { mutableStateOf("") }

    if (resumableDraft != null) {
        val live = resumableDraft.value
        // Keyed on `live` itself: re-checks whenever the observed draft
        // changes (including a late restore landing after this sheet mounted
        // with everything still blank), but never re-fires for the same
        // value. The blank-check is the non-destructive rule — a user who
        // has already typed something must never be overwritten by a
        // draft the SAME session's own edits raced ahead of.
        LaunchedEffect(live) {
            if (live != null && summary.isEmpty() && dtstart == initialDtstart &&
                dtend == initialDtend && description.isEmpty() && location.isEmpty()
            ) {
                summary = live.summary
                dtstart = live.dtstart
                dtend = live.dtend
                description = live.description
                location = live.location
            }
        }
    }

    ModalBottomSheet(onDismissRequest = onDismiss) {
        Column(
            modifier = Modifier
                .padding(horizontal = 24.dp, vertical = 16.dp)
                .fillMaxWidth()
        ) {
            Text(stringResource(R.string.events_new_event), style = MaterialTheme.typography.titleMedium)
            Spacer(Modifier.height(16.dp))

            OutlinedTextField(
                value = summary,
                onValueChange = {
                    summary = it
                    onFieldsChanged(summary, dtstart, dtend, description, location)
                },
                label = { Text(stringResource(R.string.events_summary)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.EVENT_SUMMARY)
            )
            Spacer(Modifier.height(8.dp))

            OutlinedTextField(
                value = dtstart,
                onValueChange = {
                    dtstart = it
                    onFieldsChanged(summary, dtstart, dtend, description, location)
                },
                label = { Text(stringResource(R.string.events_start_date)) },
                placeholder = { Text("YYYY-MM-DDTHH:MM") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.EVENT_DTSTART)
            )
            Spacer(Modifier.height(8.dp))

            OutlinedTextField(
                value = dtend,
                onValueChange = {
                    dtend = it
                    onFieldsChanged(summary, dtstart, dtend, description, location)
                },
                label = { Text(stringResource(R.string.events_end_date)) },
                placeholder = { Text("YYYY-MM-DDTHH:MM") },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.EVENT_DTEND)
            )
            Spacer(Modifier.height(8.dp))

            OutlinedTextField(
                value = description,
                onValueChange = {
                    description = it
                    onFieldsChanged(summary, dtstart, dtend, description, location)
                },
                label = { Text(stringResource(R.string.events_description)) },
                modifier = Modifier.fillMaxWidth().testTag(Ids.EVENT_FORM_DESCRIPTION),
                minLines = 2
            )
            Spacer(Modifier.height(8.dp))

            OutlinedTextField(
                value = location,
                onValueChange = {
                    location = it
                    onFieldsChanged(summary, dtstart, dtend, description, location)
                },
                label = { Text(stringResource(R.string.events_location)) },
                singleLine = true,
                modifier = Modifier.fillMaxWidth().testTag(Ids.EVENT_FORM_LOCATION)
            )
            Spacer(Modifier.height(16.dp))

            // The form's own completeness predicate is handed to the gate rather
            // than tested beside it, so an incomplete form keeps its own, more
            // specific reason and the gate stays silent (`FaunaGateVerdict`).
            val gate = faunaGate(
                "fauna.bridges.put_event_ciphertext",
                enabled = summary.isNotBlank() && dtstart.isNotBlank() && dtend.isNotBlank(),
            )
            Button(
                onClick = {
                    onCreate(
                        CreateEventRequest(
                            calendarId = calendarId,
                            summary = summary,
                            // Pads a bare `…THH:MM` to `…THH:MM:00` before it reaches the
                            // nest serializer (fauna_core::caltime::normalize_event_datetime_input;
                            // events.md § Where logic lives — the A2 regression).
                            dtstart = com.fauna.ffi.normalizeEventDatetimeInput(dtstart),
                            dtend = com.fauna.ffi.normalizeEventDatetimeInput(dtend),
                            description = description.ifBlank { null },
                            location = location.ifBlank { null }
                        )
                    )
                },
                enabled = gate.enabled,
                modifier = Modifier.fillMaxWidth().testTag(Ids.CREATE_EVENT)
            ) {
                Text(stringResource(R.string.events_create_event))
            }
            DisabledControlReasonText(gate.reason)
            Spacer(Modifier.height(24.dp))
        }
    }
}
