package com.fauna.app.ui.screen.events

import com.fauna.ffi.FfiCalendarViewMode
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.runtime.LaunchedEffect
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.res.stringResource
import androidx.compose.ui.unit.dp
import com.fauna.app.R
import com.fauna.app.data.api.EventSummary
import com.fauna.app.ui.components.AllDayBand
import com.fauna.app.ui.components.DayEventColumn
import com.fauna.app.ui.components.HourGutter
import com.fauna.ffi.FfiDayEvent
import com.fauna.ffi.FfiDayEventPlacement
import java.time.DayOfWeek
import java.time.LocalDate
import java.time.LocalDateTime
import social.fauna.generated.Ids

@Composable
fun WeekGridContent(
    date: LocalDate,
    events: List<EventSummary>,
    onDateChange: (LocalDate) -> Unit,
    onEventClick: (EventSummary) -> Unit,
    onSlotClick: (LocalDateTime) -> Unit,
    modifier: Modifier = Modifier,
    hourHeightDp: androidx.compose.ui.unit.Dp = 60.dp,
    // All-day/timed classification + timed geometry + overlap column-packing is
    // the one shared-Rust piece of the layout (events.md § Where logic lives);
    // injected so the Robolectric content test stays FFI-free, the same as
    // TimelineColumn.
    dayColumnLayout: (List<FfiDayEvent>) -> List<FfiDayEventPlacement> =
        { com.fauna.ffi.dayColumnLayout(it) },
) {
    // ONE locale value feeds the columns, the header and the range label.
    val firstDayOfWeek = localeFirstDayOfWeek()
    val weekStart = date.snapToWeekStart(firstDayOfWeek)
    val weekEnd = weekStart.plusDays(6)
    val daysOfWeek = (0L..6L).map { weekStart.plusDays(it) }
    val today = LocalDate.now()

    val scrollState = rememberScrollState()
    val density = LocalDensity.current
    val hourHeightPx = with(density) { hourHeightDp.toPx() }
    LaunchedEffect(Unit) {
        // Contract: auto-scroll to ~08:00 on first render.
        scrollState.scrollTo((8 * hourHeightPx).toInt())
    }

    Column(modifier = modifier.fillMaxSize().testTag(Ids.CALENDAR_WEEK_GRID)) {
        Row(
            modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically,
        ) {
            // `events-prev-month`/`events-next-month` pan the **visible range**,
            // so in week view they step a week (goal/ui/events.md § User
            // actions). The ids were missing on this screen alone — the month
            // and day screens both carry them — which left android's week view
            // the one place the shared `EventsActions.prev_month`/`next_month`
            // could not reach.
            IconButton(
                onClick = { onDateChange(date.pannedBy(FfiCalendarViewMode.WEEK, forward = false)) },
                modifier = Modifier.testTag(Ids.EVENTS_PREV_MONTH),
            ) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, "Previous week")
            }
            Text(
                weekRangeLabel(weekStart, weekEnd),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.testTag(Ids.CALENDAR_DATE_LABEL),
            )
            IconButton(
                onClick = { onDateChange(date.pannedBy(FfiCalendarViewMode.WEEK, forward = true)) },
                modifier = Modifier.testTag(Ids.EVENTS_NEXT_MONTH),
            ) {
                Icon(Icons.AutoMirrored.Filled.ArrowForward, "Next week")
            }
        }

        // Rotated to match `daysOfWeek`'s own order — both derive from the one
        // `firstDayOfWeek` above, never from a second probe. Strings stay the
        // app's i18n catalog, not a locale-dependent platform formatter, per
        // `events.md` ("localized ... name strings ... belongs to the client");
        // the locale decides only the ROTATION.
        val weekdayAbbreviations = weekdayAbbreviationsFrom(firstDayOfWeek)
        Row(modifier = Modifier.fillMaxWidth().padding(start = 52.dp)) {
            daysOfWeek.forEachIndexed { idx, day ->
                Column(modifier = Modifier.weight(1f), horizontalAlignment = Alignment.CenterHorizontally) {
                    Text(stringResource(weekdayAbbreviations[idx]), style = MaterialTheme.typography.labelSmall)
                    Text(
                        day.dayOfMonth.toString(),
                        style = if (day == today) MaterialTheme.typography.labelMedium else MaterialTheme.typography.labelSmall,
                        color = if (day == today) MaterialTheme.colorScheme.primary else MaterialTheme.colorScheme.onSurface,
                    )
                }
            }
        }

        // All-day band above the timed grid (one chip column per day).
        AllDayBand(
            days = daysOfWeek.map { day -> day to events.filter { it.dtstart.startsWith(day.toString()) } },
            onEventClick = onEventClick,
        )
        HorizontalDivider()

        // One shared DayEventColumn per day (overlap-packing, calendar-event-block
        // ids, half-hour grid, current-time line on today) behind a single shared
        // hour gutter + vertical scroll.
        Row(modifier = Modifier.fillMaxSize().verticalScroll(scrollState)) {
            HourGutter(hourHeightDp)
            daysOfWeek.forEach { day ->
                val dayEvents = events.filter { it.dtstart.startsWith(day.toString()) }
                DayEventColumn(
                    events = dayEvents,
                    hourHeightDp = hourHeightDp,
                    showCurrentTimeLine = day == today,
                    onEventClick = onEventClick,
                    // Each column binds ITS OWN date to the slot's captured time —
                    // the seven columns share one slot set but never one date.
                    onSlotClick = { slot -> onSlotClick(day.atTime(slot)) },
                    dayColumnLayout = dayColumnLayout,
                    modifier = Modifier.weight(1f),
                )
            }
        }
    }
}
