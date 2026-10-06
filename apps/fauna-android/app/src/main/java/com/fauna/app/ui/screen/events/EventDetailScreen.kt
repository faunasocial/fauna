package com.fauna.app.ui.screen.events

import androidx.compose.foundation.background
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.lazy.LazyColumn
import androidx.compose.foundation.lazy.items
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.filled.*
import androidx.compose.material3.*
import androidx.compose.runtime.*
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import androidx.hilt.navigation.compose.hiltViewModel
import androidx.navigation.NavController
import com.fauna.app.R
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.stringResourceFmt
import com.fauna.app.data.api.Attendee
import com.fauna.app.data.api.EventDetail
import com.fauna.app.data.api.EventSummary
import uniffi.fauna_core.AttendeeDisplay
import uniffi.fauna_core.LocalizedText
import uniffi.fauna_core.RsvpResponse
import com.fauna.app.ui.navigation.LocalAppMessages
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.viewmodel.EventsVM
import social.fauna.generated.Ids

/**
 * The VM-bound `event_detail` sub-page the NavHost mounts. Stateless
 * [EventDetailContent] carries every control, so the Compose test harness can
 * drive them without a ViewModel — the split batch 3 made for
 * `AccountSettingsScreen` and batch 10 for the bridge/DAV pages, done here so
 * the offline gate's nine detail-page controls are red-verifiable
 * (`account-data-plane.md` § The offline-mutation contract → *How a surface
 * asks*).
 *
 * The nav argument `eventId` is the page's identity: the detail, the attendee
 * roster and the reminder are all read with it (`EventsVM.selectEvent` seeds a
 * stub from it and calls `loadReminder(eventId)`), so every write below is
 * keyed on it too.
 */
@Composable
fun EventDetailScreen(
    navController: NavController,
    eventId: String,
    vm: EventsVM = hiltViewModel()
) {
    val selectedEvent by vm.selectedEvent.collectAsState()
    val attendees by vm.selectedEventAttendees.collectAsState()
    val currentReminder by vm.currentReminder.collectAsState()
    val isLoading by vm.isLoading.collectAsState()
    val errorMessage by vm.errorMessage.collectAsState()
    val appMessages = LocalAppMessages.current

    LaunchedEffect(eventId) {
        val stub = EventSummary(id = eventId, uid = "", summary = "", dtstart = "")
        vm.selectEvent(stub)
    }

    LaunchedEffect(errorMessage) {
        appMessages.showError(errorMessage)
    }

    EventDetailContent(
        event = selectedEvent,
        attendees = attendees,
        currentReminder = currentReminder,
        isLoading = isLoading,
        onBack = { navController.popBackStack() },
        onRsvp = { response -> vm.rsvp(eventId, response) },
        onSetReminder = { offset -> vm.setReminder(eventId, offset) },
        onRemoveReminder = { vm.removeReminder(eventId) },
        onInviteAttendee = { email -> vm.inviteAttendee(eventId, email) },
        onDeleteEvent = {
            vm.deleteEvent()
            navController.popBackStack()
        },
    )
}

/**
 * The whole `event_detail` surface, stateless (`ui/events.md` § Event detail).
 *
 * ## What declares a wire kind here
 *
 * Six controls issue a class-3 (OnlineOnly) kind and so carry a [faunaGate]
 * declaration — both RSVP buttons, the reminder Set and Remove, the attendee
 * invite (all `fauna.bridges.put_event_ciphertext`, the four VEVENT writers the
 * lead app pins in `apps/fauna-tui/src/events/mod.rs`'s fallback-free
 * `Action::wire_kind`), and the delete **confirm**
 * (`fauna.bridges.delete_event`).
 *
 * Everything else on the page is local and must survive an outage beside them:
 * the back arrow, the delete **opener** (it reveals the dialog, it does not
 * delete), the dialog's cancel, the reminder preset select — a *draft*, applied
 * on Set and never on selection — and the invite field. Greying those would
 * strand a reader on a detail page they can still read.
 *
 * ⚠ The attendee invite declares `put_event_ciphertext`, not `fauna.email.send`:
 * it persists the roster FIRST (so the roster survives a failed send) and only
 * then forks the iMIP `REQUEST` per attendee transport. The roster PUT binds the
 * ceremony; the send is offline-queued, and declaring it would leave a control
 * live that cannot persist anything.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EventDetailContent(
    event: EventDetail?,
    attendees: List<Attendee>,
    currentReminder: String?,
    isLoading: Boolean,
    onBack: () -> Unit,
    onRsvp: (response: RsvpResponse) -> Unit,
    onSetReminder: (offset: String) -> Unit,
    onRemoveReminder: () -> Unit,
    onInviteAttendee: (email: String) -> Unit,
    onDeleteEvent: () -> Unit,
) {
    var showDeleteConfirm by remember { mutableStateOf(false) }

    // Author-gating on the encrypted CalDAV path is organizer-based (events.md):
    // the actor organizes an event when its VEVENT ORGANIZER is theirs (or the
    // event is a solo one with none). Author-only affordances (delete / invite)
    // show when true; the RSVP buttons show when false (an invited event).
    val organizedByMe = event?.organizedByMe == true

    Scaffold(
        topBar = {
            TopAppBar(
                title = { Text(event?.summary ?: stringResource(R.string.events_detail_title)) },
                navigationIcon = {
                    IconButton(
                        onClick = onBack,
                        modifier = Modifier.testTag(Ids.EVENT_DETAIL_BACK)
                    ) {
                        Icon(Icons.AutoMirrored.Filled.ArrowBack, stringResource(R.string.common_back))
                    }
                }
            )
        }
    ) { padding ->
        if (isLoading && event == null) {
            Box(
                modifier = Modifier.padding(padding).fillMaxSize(),
                contentAlignment = Alignment.Center
            ) {
                CircularProgressIndicator()
            }
        } else if (event == null) {
            Box(
                modifier = Modifier.padding(padding).fillMaxSize(),
                contentAlignment = Alignment.Center
            ) {
                Text(stringResource(R.string.events_event_not_found), color = MaterialTheme.colorScheme.onSurfaceVariant)
            }
        } else {
            LazyColumn(
                modifier = Modifier.padding(padding).fillMaxSize(),
                contentPadding = PaddingValues(16.dp),
                verticalArrangement = Arrangement.spacedBy(12.dp)
            ) {
                // Detail card
                item {
                    Card(modifier = Modifier.fillMaxWidth()) {
                        Column(modifier = Modifier.padding(16.dp)) {
                            Text(event.summary, style = MaterialTheme.typography.headlineSmall,
                                modifier = Modifier.testTag(Ids.EVENT_DETAIL_SUMMARY))
                            Spacer(Modifier.height(8.dp))

                            Row(verticalAlignment = Alignment.CenterVertically) {
                                Icon(Icons.Default.DateRange, null, modifier = Modifier.size(16.dp),
                                    tint = MaterialTheme.colorScheme.onSurfaceVariant)
                                Spacer(Modifier.width(8.dp))
                                Text(
                                    buildString {
                                        append(event.dtstart)
                                        if (event.dtend != null) append(" — ${event.dtend}")
                                    },
                                    style = MaterialTheme.typography.bodyMedium,
                                    modifier = Modifier.testTag(Ids.EVENT_DETAIL_TIME)
                                )
                            }

                            if (event.location != null) {
                                Spacer(Modifier.height(4.dp))
                                Row(verticalAlignment = Alignment.CenterVertically) {
                                    Icon(Icons.Default.LocationOn, null, modifier = Modifier.size(16.dp),
                                        tint = MaterialTheme.colorScheme.onSurfaceVariant)
                                    Spacer(Modifier.width(8.dp))
                                    Text(event.location, style = MaterialTheme.typography.bodyMedium,
                                        modifier = Modifier.testTag(Ids.EVENT_DETAIL_LOCATION))
                                }
                            }

                            if (event.description != null) {
                                Spacer(Modifier.height(12.dp))
                                Text(event.description, style = MaterialTheme.typography.bodyMedium,
                                    modifier = Modifier.testTag(Ids.EVENT_DETAIL_DESCRIPTION))
                            }
                        }
                    }
                }

                // RSVP buttons (shown when the actor did not organize the event).
                // Both re-PUT the VEVENT after applying PARTSTAT locally, so both
                // declare the VEVENT-writer kind.
                if (!organizedByMe) {
                    item {
                        // One verdict for the pair: both re-PUT the VEVENT after
                        // applying PARTSTAT locally, so they issue ONE kind and
                        // carry ONE reason beneath them — per affordance-group,
                        // never a page banner (§ R11). The invited-event RSVP
                        // trio on `EventsContent` reads the same way.
                        val rsvpGate = faunaGate("fauna.bridges.put_event_ciphertext")
                        Column {
                            Row(
                                modifier = Modifier.fillMaxWidth(),
                                horizontalArrangement = Arrangement.spacedBy(8.dp)
                            ) {
                                Button(
                                    onClick = { onRsvp(RsvpResponse.GOING) },
                                    enabled = rsvpGate.enabled,
                                    modifier = Modifier.weight(1f).testTag(Ids.EVENT_DETAIL_RSVP_GOING)
                                ) {
                                    Text(stringResource(R.string.events_rsvp_going))
                                }
                                OutlinedButton(
                                    onClick = { onRsvp(RsvpResponse.DECLINED) },
                                    enabled = rsvpGate.enabled,
                                    modifier = Modifier.weight(1f).testTag(Ids.EVENT_DETAIL_RSVP_DECLINE)
                                ) {
                                    Text(stringResource(R.string.common_decline))
                                }
                            }
                            DisabledControlReasonText(rsvpGate.reason)
                        }
                    }
                }

                // Reminder section
                item {
                    Spacer(Modifier.height(16.dp))
                    Text(stringResource(R.string.events_reminder_title), style = MaterialTheme.typography.titleSmall)
                    Spacer(Modifier.height(8.dp))
                    EventReminderControl(
                        currentReminder = currentReminder,
                        onSetReminder = onSetReminder,
                        onRemoveReminder = onRemoveReminder,
                    )
                }

                // Attendees section
                if (attendees.isNotEmpty()) {
                    item {
                        Text(
                            stringResourceFmt(R.string.events_attendees_count, attendees.size.toString()),
                            style = MaterialTheme.typography.titleSmall,
                            modifier = Modifier.padding(top = 8.dp).testTag(Ids.ATTENDEE_LIST)
                        )
                    }
                    items(attendees) { attendee ->
                        AttendeeRow(attendee)
                    }
                }

                // Invite attendee form (organizer only)
                if (organizedByMe) {
                    item {
                        var inviteEmail by remember { mutableStateOf("") }

                        Card(modifier = Modifier.fillMaxWidth()) {
                            Column(modifier = Modifier.padding(16.dp)) {
                                Text(stringResource(R.string.events_invite_title), style = MaterialTheme.typography.titleSmall)
                                Spacer(Modifier.height(8.dp))

                                // Email is the universal CalDAV/iMIP attendee identifier
                                // (events.md § Scheduling): the seam adds a `mailto:` ATTENDEE
                                // + fans out an iMIP REQUEST over the outbound mail path.
                                // Cross-nest mailbox-less-Fauna delivery is fully automatic —
                                // resolved from the CAL-ADDRESS alone via anon by_handle
                                // discovery, no manual nest URL.
                                //
                                // The field itself is a local buffer and stays live offline:
                                // a user may compose the invitation and send it on reconnect.
                                OutlinedTextField(
                                    value = inviteEmail,
                                    onValueChange = { inviteEmail = it },
                                    label = { Text(stringResource(R.string.events_invite_email_label)) },
                                    placeholder = { Text(stringResource(R.string.events_invite_email_placeholder)) },
                                    singleLine = true,
                                    modifier = Modifier.fillMaxWidth().testTag(Ids.ATTENDEE_INVITE_FIELD)
                                )
                                Spacer(Modifier.height(8.dp))

                                val inviteGate = faunaGate(
                                    "fauna.bridges.put_event_ciphertext",
                                    enabled = inviteEmail.isNotBlank(),
                                )
                                Button(
                                    onClick = {
                                        onInviteAttendee(inviteEmail)
                                        inviteEmail = ""
                                    },
                                    enabled = inviteGate.enabled,
                                    modifier = Modifier.testTag(Ids.ATTENDEE_INVITE_BUTTON)
                                ) {
                                    Text(stringResource(R.string.events_send_invite))
                                }
                                DisabledControlReasonText(inviteGate.reason)
                            }
                        }
                    }
                }

                // Delete button (organizer only). The opener only REVEALS the
                // confirmation — it deletes nothing, so it stays live with no
                // nest and is the live sibling the gate test pairs the confirm
                // against.
                if (organizedByMe) {
                    item {
                        Spacer(Modifier.height(16.dp))
                        OutlinedButton(
                            onClick = { showDeleteConfirm = true },
                            colors = ButtonDefaults.outlinedButtonColors(
                                contentColor = MaterialTheme.colorScheme.error
                            ),
                            modifier = Modifier.fillMaxWidth().testTag(Ids.EVENT_DELETE_BTN)
                        ) {
                            Icon(Icons.Default.Delete, null, modifier = Modifier.size(18.dp))
                            Spacer(Modifier.width(8.dp))
                            Text(stringResource(R.string.events_delete_event))
                        }
                    }
                }
            }
        }
    }

    // Delete confirmation dialog. A Material `AlertDialog`'s slots are ordinary
    // composables in the same composition, so the gate inside reads
    // `LocalConnectionState` exactly as the page around it does.
    if (showDeleteConfirm) {
        AlertDialog(
            onDismissRequest = { showDeleteConfirm = false },
            title = { Text(stringResource(R.string.events_delete_event)) },
            text = { Text(stringResource(R.string.events_delete_event_confirm)) },
            confirmButton = {
                val deleteGate = faunaGate("fauna.bridges.delete_event")
                Column {
                    TextButton(
                        onClick = {
                            showDeleteConfirm = false
                            onDeleteEvent()
                        },
                        enabled = deleteGate.enabled,
                    ) {
                        Text(stringResource(R.string.common_delete), color = MaterialTheme.colorScheme.error)
                    }
                    DisabledControlReasonText(deleteGate.reason)
                }
            },
            dismissButton = {
                // Backing out of a confirmation is pure local UI and must not
                // become unreachable because the nest went away.
                TextButton(onClick = { showDeleteConfirm = false }) { Text(stringResource(R.string.common_cancel)) }
            }
        )
    }
}

/**
 * The shared `event-reminder` control (`ui/events.md` § Reminders), in its two
 * ratified states: with **no reminder set** a preset select
 * (`event-detail-reminder-select`) plus an explicit **Set**
 * (`event-detail-reminder-set`); with one set, the current-offset label
 * (`event-detail-reminder-current`) plus **Remove**
 * (`event-detail-reminder-remove`).
 *
 * ⚠ **The preset is applied on Set, never on selection.** android used to auto-
 * apply on chip tap — the same deviation iOS's `onChange` variant was, resolved
 * against this spec on 2026-06-24 — which left the page with no
 * `event-detail-reminder-*` ids at all and made the cross-app driver contract
 * (`select(id, "PT1H")` **then** `click(event-detail-reminder-set)`)
 * unexecutable here. It now matches tui, linux and web (priority #1/#4).
 *
 * Because the select is a *draft* it issues no wire kind and carries no gate —
 * the lead app pins that explicitly (`Action::SetReminderOffset` → `None`). Set
 * and Remove are both re-PUTs of the VEVENT, so both declare the writer kind.
 */
@OptIn(ExperimentalMaterial3Api::class)
@Composable
fun EventReminderControl(
    currentReminder: String?,
    onSetReminder: (offset: String) -> Unit,
    onRemoveReminder: () -> Unit,
    // The shared preset catalog + offset→label map (`fauna_core::ical::
    // reminder_presets` / `reminder_label` over UniFFI, events.md § Reminders),
    // injected so a Robolectric content-test can seed them without the native
    // library — the same shape `AttendeeRow`'s `view` and `AdminCalendarContent`'s
    // `parsePort` take. Never a hand-rolled Kotlin value array.
    presets: List<Pair<String, LocalizedText>> = remember {
        com.fauna.ffi.reminderPresets().map { it.value to it.label }
    },
    reminderLabel: (String) -> LocalizedText = { com.fauna.ffi.reminderLabel(it) },
) {
    // The two states are mutually exclusive, spelled as one if/else rather than
    // an early return so the composable has a single exit and a reader can see
    // at a glance that Set and Remove never render together.
    if (currentReminder != null) {
        Column {
            Text(
                localized(reminderLabel(currentReminder)).orEmpty(),
                style = MaterialTheme.typography.bodyMedium,
                modifier = Modifier.testTag(Ids.EVENT_DETAIL_REMINDER_CURRENT),
            )
            Spacer(Modifier.height(8.dp))
            val removeGate = faunaGate("fauna.bridges.put_event_ciphertext")
            TextButton(
                onClick = onRemoveReminder,
                enabled = removeGate.enabled,
                modifier = Modifier.testTag(Ids.EVENT_DETAIL_REMINDER_REMOVE),
            ) {
                Text(stringResource(R.string.events_remove_reminder))
            }
            DisabledControlReasonText(removeGate.reason)
        }
    } else {
        var expanded by remember { mutableStateOf(false) }
        // The draft: the ISO-8601 offset the select holds until Set applies it. The
        // catalog's first entry is the picker's default, matching every other app's
        // dropdown, which opens on its first row rather than on nothing.
        var draft by remember(presets) { mutableStateOf(presets.firstOrNull()?.first.orEmpty()) }

        Column {
            ExposedDropdownMenuBox(
                expanded = expanded,
                onExpandedChange = { expanded = !expanded },
            ) {
                OutlinedTextField(
                    value = presets.firstOrNull { it.first == draft }
                        ?.let { localized(it.second).orEmpty() }
                        ?: stringResource(R.string.events_reminder_select_placeholder),
                    onValueChange = {},
                    readOnly = true,
                    label = { Text(stringResource(R.string.events_reminder_title)) },
                    trailingIcon = { ExposedDropdownMenuDefaults.TrailingIcon(expanded = expanded) },
                    modifier = Modifier
                        .menuAnchor()
                        .fillMaxWidth()
                        .testTag(Ids.EVENT_DETAIL_REMINDER_SELECT),
                )
                ExposedDropdownMenu(expanded = expanded, onDismissRequest = { expanded = false }) {
                    presets.forEach { (value, label) ->
                        DropdownMenuItem(
                            text = { Text(localized(label).orEmpty()) },
                            onClick = { draft = value; expanded = false },
                        )
                    }
                }
            }
            Spacer(Modifier.height(8.dp))
            val setGate = faunaGate(
                "fauna.bridges.put_event_ciphertext",
                enabled = draft.isNotBlank(),
            )
            Button(
                onClick = { onSetReminder(draft) },
                enabled = setGate.enabled,
                modifier = Modifier.testTag(Ids.EVENT_DETAIL_REMINDER_SET),
            ) {
                Text(stringResource(R.string.events_reminder_set))
            }
            DisabledControlReasonText(setGate.reason)
        }
    }
}

/**
 * One `attendee-item` row on the event detail (`events.md` § Attendee list
 * presentation, ratified 2026-06-23): a generated **monogram avatar** (the
 * uppercased initial of the display name — attendees carry no avatar URL, a CalDAV
 * ATTENDEE is just CN + email + PARTSTAT, so the monogram is generated never
 * fetched) + the **display name** (CN, falling back to the bare email) + the
 * **email** beneath (omitted when the name IS the email, i.e. no CN) + a trailing
 * **colored RSVP-status indicator**. The status→color map mirrors apple FaunaKit
 * `rsvpStatusColor` (priority #1/#4). Stateless for the Robolectric harness.
 */
@Composable
fun AttendeeRow(
    attendee: Attendee,
    modifier: Modifier = Modifier,
    // Shared text projection (CN→email fallback, generated monogram initial,
    // email-beneath visibility): `fauna_core::ical::attendee_display` via UniFFI —
    // one source of truth across all 7 apps (events.md § Attendee list
    // presentation). Injected as a default so the stateless Robolectric
    // content-test seeds it without the native lib (android Robolectric never calls
    // real FFI — see ValueFormatResolverTest).
    view: AttendeeDisplay = com.fauna.ffi.attendeeDisplay(attendee.name, attendee.email),
    // Shared status→label text: `fauna_core::ical::rsvp_status_label` via UniFFI,
    // resolved through the android i18n pipeline (`events.rsvp.*` → string resource;
    // an unknown status returns its capitalized-verbatim form as the key, which has
    // no resource so it renders as-is — preserving the prior per-app capitalize).
    // One source of truth across all 7 apps (events.md § Attendee list
    // presentation) — only the LABEL is shared; the trailing color stays a
    // per-platform idiomatic map (`rsvpStatusColor`). Injected as a default
    // (mirroring `view`) so the Robolectric content-test seeds it FFI-free.
    statusLabel: String = localized(com.fauna.ffi.rsvpStatusLabel(attendee.rsvp)).orEmpty(),
) {
    val statusColor = rsvpStatusColor(attendee.rsvp)

    ListItem(
        modifier = modifier.testTag(Ids.ATTENDEE_ITEM),
        leadingContent = {
            Box(
                modifier = Modifier
                    .size(36.dp)
                    .clip(CircleShape)
                    .background(MaterialTheme.colorScheme.primaryContainer),
                contentAlignment = Alignment.Center,
            ) {
                Text(
                    view.monogram,
                    style = MaterialTheme.typography.labelLarge,
                    color = MaterialTheme.colorScheme.onPrimaryContainer,
                )
            }
        },
        headlineContent = { Text(view.displayName, modifier = Modifier.testTag(Ids.ATTENDEE_ID)) },
        // Email beneath the name — only when the name is a real, distinct CN.
        supportingContent = view.secondaryEmail?.let { secondary -> { Text(secondary) } },
        trailingContent = {
            Text(
                statusLabel,
                color = statusColor,
                style = MaterialTheme.typography.labelMedium,
                modifier = Modifier.testTag(Ids.ATTENDEE_STATUS),
            )
        },
    )
}

/**
 * The attendee RSVP-status → trailing **color**, an idiomatic per-platform map
 * mirroring apple FaunaKit `rsvpStatusColor` (`events.md` § Attendee list
 * presentation): going → green, interested → yellow, declined → red, waitlisted →
 * orange, invited / unknown (incl. the verbatim `tentative`) → secondary.
 * `@Composable` for the theme color. The **label text** is deliberately NOT here:
 * it is single-sourced in shared Rust (`fauna_core::ical::rsvp_status_label`) and
 * reaches the row via the injected `statusLabel` param (events.md § Attendee list
 * presentation — "the label can't drift per-app" while "the color is rendered
 * idiomatically per client"). The color is deliberately not lifted (re-encoding
 * the status would earn no sharing).
 */
@Composable
private fun rsvpStatusColor(rsvp: String): Color = when (rsvp) {
    "going" -> Color(0xFF4CAF50)
    "interested" -> Color(0xFFFBC02D)
    "declined" -> Color(0xFFF44336)
    "waitlisted" -> Color(0xFFFF9800)
    // invited / tentative / unknown → secondary
    else -> MaterialTheme.colorScheme.onSurfaceVariant
}
