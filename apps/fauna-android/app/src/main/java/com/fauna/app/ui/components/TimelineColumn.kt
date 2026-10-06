package com.fauna.app.ui.components

import androidx.compose.foundation.Canvas
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.rememberScrollState
import androidx.compose.foundation.verticalScroll
import androidx.compose.material3.Card
import androidx.compose.material3.CardDefaults
import androidx.compose.material3.MaterialTheme
import androidx.compose.material3.Surface
import androidx.compose.material3.Text
import androidx.compose.runtime.*
import androidx.compose.ui.Modifier
import androidx.compose.ui.geometry.Offset
import androidx.compose.ui.graphics.Color
import androidx.compose.ui.layout.Layout
import androidx.compose.ui.platform.LocalDensity
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.text.style.TextOverflow
import androidx.compose.ui.unit.Dp
import androidx.compose.ui.unit.dp
import com.fauna.app.data.api.EventSummary
import com.fauna.ffi.FfiDayEvent
import com.fauna.ffi.FfiDayEventPlacement
import java.time.LocalDate
import java.time.LocalDateTime
import java.time.LocalTime
import java.time.format.DateTimeFormatter
import social.fauna.generated.Ids

private val HHMM = DateTimeFormatter.ofPattern("HH:mm")

/**
 * The empty-slot quick-create targets tiling one day column: 96 fixed 15-minute
 * slots (ui.yaml `events-time-slot-{HH-MM}`, indexed by the slot's snapped start
 * time; events.md § Week & day timeline views). Computed once at class init —
 * the same set backs every column, day view and each of the week's 7 alike.
 */
private val QUARTER_HOUR_SLOTS: List<LocalTime> =
    (0 until 96).map { LocalTime.of(it / 4, (it % 4) * 15) }

/** `HH-MM`, the id suffix form of [QUARTER_HOUR_SLOTS] (not the `HH:mm` label form). */
private val SLOT_ID_HHMM = DateTimeFormatter.ofPattern("HH-mm")

/**
 * The day-view time grid (events.md § Week & day timeline views): the left hour
 * gutter + a single [DayEventColumn], wrapped in a vertical scroll that
 * auto-scrolls to ~08:00 on first render (the contract default, matching
 * web/windows/linux). The all-day band lives in the parent screen
 * ([com.fauna.app.ui.screen.events.DayTimelineContent]), above this grid.
 *
 * All-day/timed classification, timed minute geometry (start/end clamped into
 * the day — no cross-midnight overflow), and overlap column-packing are one
 * shared-Rust call, `fauna_core::caltime::day_column_layout` (UniFFI
 * `dayColumnLayout`), injected so the Robolectric content test stays
 * deterministic and FFI-free (mirrors how AttendeeRow injects
 * `attendee_display`'s `view`). The same [DayEventColumn] renderer backs each
 * of the week grid's 7 columns (priority #2/#4 — one renderer for both views).
 */
@Composable
fun TimelineColumn(
    events: List<EventSummary>,
    hourHeightDp: Dp = 60.dp,
    onEventClick: (EventSummary) -> Unit,
    onSlotClick: (LocalTime) -> Unit,
    showCurrentTimeLine: Boolean = false,
    modifier: Modifier = Modifier,
    dayColumnLayout: (List<FfiDayEvent>) -> List<FfiDayEventPlacement> =
        { com.fauna.ffi.dayColumnLayout(it) },
) {
    val scrollState = rememberScrollState()
    val density = LocalDensity.current
    val hourHeightPx = with(density) { hourHeightDp.toPx() }

    LaunchedEffect(Unit) {
        // Contract: auto-scroll to ~08:00 on first render.
        scrollState.scrollTo((8 * hourHeightPx).toInt())
    }

    Row(modifier = modifier.verticalScroll(scrollState)) {
        HourGutter(hourHeightDp)
        DayEventColumn(
            events = events,
            hourHeightDp = hourHeightDp,
            showCurrentTimeLine = showCurrentTimeLine,
            onEventClick = onEventClick,
            onSlotClick = onSlotClick,
            dayColumnLayout = dayColumnLayout,
            modifier = Modifier.weight(1f),
        )
    }
}

/** The shared left hour gutter (00:00–23:00), 24 rows of `hourHeightDp`. */
@Composable
fun HourGutter(hourHeightDp: Dp, modifier: Modifier = Modifier) {
    Column(modifier = modifier.width(52.dp)) {
        for (hour in 0..23) {
            Box(modifier = Modifier.height(hourHeightDp)) {
                Text(
                    String.format("%02d:00", hour),
                    style = MaterialTheme.typography.labelSmall,
                    color = MaterialTheme.colorScheme.onSurfaceVariant,
                    modifier = Modifier.padding(start = 4.dp, top = 2.dp),
                )
            }
        }
    }
}

/**
 * One day's timed-event grid: a 24h-tall background of **half-hour** grid lines,
 * the 96 empty-slot quick-create targets, the day's **timed** events as
 * positioned blocks (top/height/left/width all from the shared
 * `day_column_layout` placement — start/end minutes clamped into the day and the
 * packed overlap column/count), and a `calendar-current-time` line when
 * [showCurrentTimeLine] (today's column). All-day events are excluded here —
 * they render in the [AllDayBand]. Carries no gutter and no scroll; the caller
 * (day view or one of the week's 7 columns) provides those.
 *
 * [onSlotClick] fires with the clicked slot's own **closure-captured** time, and
 * the caller pairs it with its column's date (`WeekGridContent` per column,
 * `DayTimelineContent` for the single one) — a time re-derived from the pointer's
 * y would read 00:00 under every headless driver, which emits its gesture at
 * (0,0). Block-vs-slot precedence is structural: the blocks are added to the Box
 * *after* the slot targets and Compose hit-tests topmost-first, so a click landing
 * on an event opens the event and never also quick-creates beneath it (the same
 * child-order solution as linux/apple/windows, no event-consumption trick). The
 * 96-per-column cost (672 across the week grid) is the same one windows weighed
 * and accepted: they are empty, transparent, built once per column render.
 */
@Composable
fun DayEventColumn(
    events: List<EventSummary>,
    hourHeightDp: Dp = 60.dp,
    showCurrentTimeLine: Boolean = false,
    onEventClick: (EventSummary) -> Unit,
    onSlotClick: (LocalTime) -> Unit,
    modifier: Modifier = Modifier,
    dayColumnLayout: (List<FfiDayEvent>) -> List<FfiDayEventPlacement> =
        { com.fauna.ffi.dayColumnLayout(it) },
) {
    val density = LocalDensity.current
    val hourHeightPx = with(density) { hourHeightDp.toPx() }
    val gapPx = with(density) { 2.dp.toPx() }.toInt()
    val columnHeight = hourHeightDp * 24
    val lineColor = MaterialTheme.colorScheme.outlineVariant
    val currentTimeColor = Color.Red

    // One placement per input event, in input order (shared Rust owns all-day
    // classification, the timed minute geometry, and the overlap column packing
    // in a single call — never re-derive any of the three locally).
    val placements: List<FfiDayEventPlacement> = if (events.isEmpty()) {
        emptyList()
    } else {
        dayColumnLayout(events.map { FfiDayEvent(start = it.dtstart, end = it.dtend) })
    }
    // Only timed events lay out in the grid; all-day events live in the band.
    val timed: List<Pair<EventSummary, FfiDayEventPlacement>> =
        events.zip(placements).filter { (_, placement) -> !placement.allDay }

    Box(modifier = modifier.height(columnHeight)) {
        // Half-hour grid lines (contract: half-hour row granularity). The on-the-hour
        // lines are solid; the half-hour lines are lighter.
        Canvas(modifier = Modifier.fillMaxSize()) {
            val halfHourPx = hourHeightPx / 2f
            for (i in 0..48) {
                val y = i * halfHourPx
                val onHour = i % 2 == 0
                drawLine(
                    color = if (onHour) lineColor else lineColor.copy(alpha = 0.4f),
                    start = Offset(0f, y),
                    end = Offset(size.width, y),
                    strokeWidth = if (onHour) 1f else 0.5f,
                )
            }
        }

        // The quick-create targets, tiled over the (non-interactive) grid lines
        // and UNDER the event blocks added below. Each slot is exactly a quarter
        // of an hour tall, so the 96 of them tile the column's full height
        // exactly (96 * hourHeightDp/4 == 24 * hourHeightDp == columnHeight).
        Column {
            QUARTER_HOUR_SLOTS.forEach { slot ->
                Box(
                    modifier = Modifier
                        .fillMaxWidth()
                        .height(hourHeightDp / 4)
                        .testTag("events-time-slot-${slot.format(SLOT_ID_HHMM)}")
                        .clickable { onSlotClick(slot) },
                )
            }
        }

        Layout(
            content = {
                timed.forEach { (event, _) ->
                    val startTime = parseEventTime(event.dtstart)
                    val endTime = if (event.dtend != null) parseEventTime(event.dtend) else startTime.plusHours(1)

                    Card(
                        modifier = Modifier.testTag(Ids.CALENDAR_EVENT_BLOCK).clickable { onEventClick(event) },
                        colors = CardDefaults.cardColors(
                            containerColor = MaterialTheme.colorScheme.primaryContainer,
                        ),
                    ) {
                        Column(modifier = Modifier.padding(4.dp)) {
                            Text(event.summary, style = MaterialTheme.typography.labelSmall, maxLines = 2)
                            Text(
                                "${startTime.toLocalTime().format(HHMM)} - ${endTime.toLocalTime().format(HHMM)}",
                                style = MaterialTheme.typography.labelSmall,
                                color = MaterialTheme.colorScheme.onSurfaceVariant,
                            )
                        }
                    }
                }
            },
            modifier = Modifier.fillMaxSize(),
        ) { measurables, constraints ->
            val placeables = measurables.mapIndexed { index, measurable ->
                val placement = timed[index].second
                val heightPx = ((placement.endMin.toInt() - placement.startMin.toInt()) / 60f * hourHeightPx).toInt()
                val totalCols = placement.totalColumns.toInt().coerceAtLeast(1)
                val blockWidth = (constraints.maxWidth / totalCols - gapPx).coerceAtLeast(1)
                measurable.measure(
                    constraints.copy(
                        minWidth = blockWidth, maxWidth = blockWidth,
                        minHeight = heightPx, maxHeight = heightPx,
                    ),
                )
            }
            layout(constraints.maxWidth, (24 * hourHeightPx).toInt()) {
                placeables.forEachIndexed { index, placeable ->
                    val placement = timed[index].second
                    val y = (placement.startMin.toInt() / 60f * hourHeightPx).toInt()
                    val total = placement.totalColumns.toInt().coerceAtLeast(1)
                    val x = placement.columnIndex.toInt() * (constraints.maxWidth / total)
                    placeable.place(x, y)
                }
            }
        }

        if (showCurrentTimeLine) {
            val now = LocalTime.now()
            val minutesSinceMidnight = now.hour * 60 + now.minute
            val yOffset = hourHeightDp * (minutesSinceMidnight / 60f)

            Canvas(modifier = Modifier.testTag(Ids.CALENDAR_CURRENT_TIME).fillMaxWidth().offset(y = yOffset)) {
                drawLine(
                    color = currentTimeColor, start = Offset(0f, 0f),
                    end = Offset(size.width, 0f), strokeWidth = 2f,
                )
                drawCircle(color = currentTimeColor, radius = 4f, center = Offset(4f, 0f))
            }
        }
    }
}

/**
 * The all-day band (`calendar-allday-band`) above the timed grid (day + week
 * views), carrying per-day all-day-event chips. [days] is one entry per visible
 * day (1 for the day view, 7 for the week), each with that day's events; only the
 * all-day ones (`fauna_core::caltime::is_all_day`, UniFFI `eventIsAllDay`) render
 * as clickable chips, column-aligned under the day headers via a leading
 * [gutterWidth] spacer + one weight-1f cell per day. The band always renders
 * (even empty) so the contract id is always present.
 */
@Composable
fun AllDayBand(
    days: List<Pair<LocalDate, List<EventSummary>>>,
    onEventClick: (EventSummary) -> Unit,
    modifier: Modifier = Modifier,
    gutterWidth: Dp = 52.dp,
) {
    Row(modifier = modifier.fillMaxWidth().testTag(Ids.CALENDAR_ALLDAY_BAND)) {
        Spacer(Modifier.width(gutterWidth))
        days.forEach { (_, dayEvents) ->
            Column(
                modifier = Modifier.weight(1f).padding(horizontal = 1.dp, vertical = 2.dp),
                verticalArrangement = Arrangement.spacedBy(2.dp),
            ) {
                dayEvents.filter { com.fauna.ffi.eventIsAllDay(it.dtstart, it.dtend) }.forEach { event ->
                    Surface(
                        modifier = Modifier.fillMaxWidth().clickable { onEventClick(event) },
                        color = MaterialTheme.colorScheme.secondaryContainer,
                        shape = MaterialTheme.shapes.small,
                    ) {
                        Text(
                            event.summary,
                            style = MaterialTheme.typography.labelSmall,
                            maxLines = 1,
                            overflow = TextOverflow.Ellipsis,
                            modifier = Modifier.padding(horizontal = 4.dp, vertical = 2.dp),
                        )
                    }
                }
            }
        }
    }
}

private fun parseEventTime(dtstring: String): LocalDateTime {
    return try {
        LocalDateTime.parse(dtstring.replace(" ", "T").take(19))
    } catch (_: Exception) {
        try {
            LocalDate.parse(dtstring.take(10)).atStartOfDay()
        } catch (_: Exception) {
            LocalDateTime.now()
        }
    }
}
