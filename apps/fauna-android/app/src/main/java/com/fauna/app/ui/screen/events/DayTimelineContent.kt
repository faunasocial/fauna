package com.fauna.app.ui.screen.events

import com.fauna.ffi.FfiCalendarViewMode
import androidx.compose.foundation.layout.*
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import com.fauna.app.data.api.EventSummary
import com.fauna.app.ui.components.AllDayBand
import com.fauna.app.ui.components.TimelineColumn
import java.time.LocalDate
import java.time.LocalDateTime
import social.fauna.generated.Ids

@Composable
fun DayTimelineContent(
    date: LocalDate,
    events: List<EventSummary>,
    onDateChange: (LocalDate) -> Unit,
    onEventClick: (EventSummary) -> Unit,
    onSlotClick: (LocalDateTime) -> Unit,
    modifier: Modifier = Modifier
) {
    Column(modifier = modifier.fillMaxSize().testTag(Ids.CALENDAR_DAY_TIMELINE)) {
        Row(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically) {
            IconButton(
                onClick = { onDateChange(date.pannedBy(FfiCalendarViewMode.DAY, forward = false)) },
                modifier = Modifier.testTag(Ids.EVENTS_PREV_MONTH)
            ) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, "Previous day")
            }
            Text(fullDayLabel(date),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.testTag(Ids.CALENDAR_DATE_LABEL))
            IconButton(
                onClick = { onDateChange(date.pannedBy(FfiCalendarViewMode.DAY, forward = true)) },
                modifier = Modifier.testTag(Ids.EVENTS_NEXT_MONTH)
            ) {
                Icon(Icons.AutoMirrored.Filled.ArrowForward, "Next day")
            }
        }
        AllDayBand(days = listOf(date to events), onEventClick = onEventClick)
        HorizontalDivider()
        TimelineColumn(events = events, onEventClick = onEventClick,
            // The single column IS this view's date, so the slot's own captured
            // time pairs with it unambiguously (events.md § Week & day timeline
            // views — the empty-slot Outlook drill-in).
            onSlotClick = { slot -> onSlotClick(date.atTime(slot)) },
            showCurrentTimeLine = date == LocalDate.now(),
            modifier = Modifier.fillMaxSize())
    }
}
