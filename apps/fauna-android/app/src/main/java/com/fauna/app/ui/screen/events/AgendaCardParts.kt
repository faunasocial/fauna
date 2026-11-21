package com.fauna.app.ui.screen.events

import androidx.compose.foundation.layout.Arrangement
import androidx.compose.foundation.layout.Column
import androidx.compose.foundation.layout.Row
import androidx.compose.foundation.layout.fillMaxWidth
import androidx.compose.foundation.layout.padding
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material3.Icon
import androidx.compose.material3.IconButton
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Text
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalContext
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import uniffi.fauna_core.RsvpResponse
import com.fauna.app.ui.components.DisabledControlReasonText
import com.fauna.app.ui.util.blockedReason
import com.fauna.app.ui.util.faunaGate
import com.fauna.app.ui.util.isPossible
import com.fauna.app.ui.util.localized
import com.fauna.app.ui.util.paintedReason
import com.fauna.app.ui.util.pressAsked
import com.fauna.ffi.FfiCalendarViewMode
import com.fauna.ffi.calendarPanPossibility
import java.time.LocalDate
import social.fauna.generated.Ids
import com.fauna.app.ui.controls.Button
import com.fauna.app.ui.controls.OutlinedButton

/**
 * The card-level `event-rsvp-going` / `-interested` / `-decline` trio on one
 * agenda `event-card` (`docs/goal/ui/events.md` § components — the
 * `rsvp-button-group` is a child of BOTH `event-card` and `event_detail`).
 *
 * It submits through the same `onRsvp` the detail page uses (`EventsVM.rsvp`),
 * so the card and detail cannot drift — the linux lesson of 2026-07-31, where
 * the card buttons were tagged but never wired. Rendered once per agenda row in
 * the same `items(events)` order as `event-card-summary`, which is the index
 * contract the e2e `rsvp_on_card` helper relies on.
 *
 * Every RSVP re-PUTs the VEVENT after applying PARTSTAT locally, so all three
 * declare the VEVENT-writer kind — one gate verdict for the trio.
 */
@Composable
fun EventCardRsvpButtons(
    eventId: String,
    onRsvp: (eventId: String, response: RsvpResponse) -> Unit,
    modifier: Modifier = Modifier,
) {
    val rsvpGate = faunaGate("fauna.bridges.put_event_ciphertext")
    Column(modifier = modifier) {
        Row(horizontalArrangement = Arrangement.spacedBy(8.dp)) {
            Button(
                onClick = { onRsvp(eventId, RsvpResponse.GOING) },
                enabled = rsvpGate.enabled,
                modifier = Modifier.testTag(Ids.EVENT_RSVP_GOING),
            ) {
                Text(stringResource(R.string.events_rsvp_going))
            }
            OutlinedButton(
                onClick = { onRsvp(eventId, RsvpResponse.INTERESTED) },
                enabled = rsvpGate.enabled,
                modifier = Modifier.testTag(Ids.EVENT_RSVP_INTERESTED),
            ) {
                Text(stringResource(R.string.events_rsvp_interested))
            }
            OutlinedButton(
                onClick = { onRsvp(eventId, RsvpResponse.DECLINED) },
                enabled = rsvpGate.enabled,
                modifier = Modifier.testTag(Ids.EVENT_RSVP_DECLINE),
            ) {
                Text(stringResource(R.string.common_decline))
            }
        }
        DisabledControlReasonText(rsvpGate.reason)
    }
}

/**
 * Agenda view's `events-prev-month` / `calendar-date-label` /
 * `events-next-month` header — present like every other view's, and **disabled
 * with its reason**.
 *
 * The agenda list is date-unfiltered, so the shared `pan_step` answers `None`
 * for it (`events.md` § Where logic lives → *View mode + visible range*) and a
 * pan would do nothing. The pair is painted from the shared
 * `caltime::pan_possibility` (`calendarPanPossibility`, blocked with
 * `events.pan_none_in_agenda`): disabled, the reason within eyeshot and
 * published as the buttons' `reason` attribute, never withdrawn
 * (`ui/README.md` § Offer only what is possible, rule 6). A pan that still
 * reaches the handler asks the SAME answer again ([pressAsked]) and refuses
 * with its reason through [onPanRefused] (the page's `error-message`); only a
 * possible answer goes on through [pannedBy], so if the shared Rust ever gave
 * agenda a range this header would follow without an android edit. The label
 * reads `Upcoming`, as on windows and apple, since
 * there is no range to name.
 */
@Composable
fun AgendaPanHeader(
    date: LocalDate,
    onDateChange: (LocalDate) -> Unit,
    modifier: Modifier = Modifier,
    onPanRefused: (String) -> Unit = {},
) {
    val context = LocalContext.current
    val answer = calendarPanPossibility(FfiCalendarViewMode.AGENDA)
    val reason = answer.blockedReason?.let { localized(it) }
    fun pan(forward: Boolean) = pressAsked(context, answer, onPanRefused) {
        onDateChange(date.pannedBy(FfiCalendarViewMode.AGENDA, forward = forward))
    }
    Column(modifier = modifier.fillMaxWidth()) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            IconButton(
                onClick = { pan(forward = false) },
                enabled = answer.isPossible,
                modifier = Modifier.testTag(Ids.EVENTS_PREV_MONTH).paintedReason(reason),
            ) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, contentDescription = null)
            }
            Text(
                stringResource(R.string.common_upcoming),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.testTag(Ids.CALENDAR_DATE_LABEL),
            )
            IconButton(
                onClick = { pan(forward = true) },
                enabled = answer.isPossible,
                modifier = Modifier.testTag(Ids.EVENTS_NEXT_MONTH).paintedReason(reason),
            ) {
                Icon(Icons.AutoMirrored.Filled.ArrowForward, contentDescription = null)
            }
        }
        DisabledControlReasonText(reason, Modifier.padding(horizontal = 16.dp))
    }
}
