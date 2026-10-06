package com.fauna.app.ui.screen.events

import com.fauna.ffi.FfiCalendarViewMode
import androidx.compose.foundation.background
import androidx.compose.foundation.clickable
import androidx.compose.foundation.layout.*
import androidx.compose.foundation.shape.CircleShape
import androidx.compose.material.icons.Icons
import androidx.compose.material.icons.automirrored.filled.ArrowBack
import androidx.compose.material.icons.automirrored.filled.ArrowForward
import androidx.compose.material3.*
import androidx.compose.runtime.Composable
import androidx.compose.ui.Alignment
import androidx.compose.ui.Modifier
import androidx.compose.ui.draw.clip
import androidx.compose.ui.platform.testTag
import androidx.compose.ui.unit.dp
import androidx.compose.ui.res.stringResource
import com.fauna.app.R
import com.fauna.app.data.api.EventSummary
import java.time.DayOfWeek
import java.time.LocalDate
import java.time.YearMonth
import social.fauna.generated.Ids

@Composable
fun MonthGridContent(
    date: LocalDate,
    events: List<EventSummary>,
    onDateChange: (LocalDate) -> Unit,
    onDayClick: (LocalDate) -> Unit,
    modifier: Modifier = Modifier
) {
    val yearMonth = YearMonth.from(date)
    val firstOfMonth = yearMonth.atDay(1)
    // ONE locale value feeds the grid and the header below it, so the two
    // cannot name different weeks (events.md § Week & day timeline views).
    val firstDayOfWeek = localeFirstDayOfWeek()
    val gridStart = firstOfMonth.snapToWeekStart(firstDayOfWeek)
    val days = (0 until 42).map { gridStart.plusDays(it.toLong()) }
    val eventsByDate = events.groupBy { it.dtstart.take(10) }

    Column(modifier = modifier.fillMaxSize().testTag(Ids.EVENTS_MONTH_GRID)) {
        Row(modifier = Modifier.fillMaxWidth().padding(horizontal = 16.dp, vertical = 8.dp),
            horizontalArrangement = Arrangement.SpaceBetween,
            verticalAlignment = Alignment.CenterVertically) {
            IconButton(
                onClick = { onDateChange(date.pannedBy(FfiCalendarViewMode.MONTH, forward = false)) },
                modifier = Modifier.testTag(Ids.EVENTS_PREV_MONTH)
            ) {
                Icon(Icons.AutoMirrored.Filled.ArrowBack, "Previous month")
            }
            Text(monthYearLabel(date),
                style = MaterialTheme.typography.titleMedium,
                modifier = Modifier.testTag(Ids.CALENDAR_DATE_LABEL))
            IconButton(
                onClick = { onDateChange(date.pannedBy(FfiCalendarViewMode.MONTH, forward = true)) },
                modifier = Modifier.testTag(Ids.EVENTS_NEXT_MONTH)
            ) {
                Icon(Icons.AutoMirrored.Filled.ArrowForward, "Next month")
            }
        }

        Row(modifier = Modifier.fillMaxWidth()) {
            weekdayAbbreviationsFrom(firstDayOfWeek).forEach { dayRes ->
                Box(modifier = Modifier.weight(1f), contentAlignment = Alignment.Center) {
                    Text(stringResource(dayRes), style = MaterialTheme.typography.labelSmall,
                        color = MaterialTheme.colorScheme.onSurfaceVariant)
                }
            }
        }
        Spacer(Modifier.height(4.dp))

        for (week in 0 until 6) {
            Row(modifier = Modifier.fillMaxWidth()) {
                for (dayIdx in 0 until 7) {
                    val day = days[week * 7 + dayIdx]
                    val isCurrentMonth = day.month == yearMonth.month
                    val isToday = day == LocalDate.now()
                    val dayEvents = eventsByDate[day.toString()] ?: emptyList()

                    Box(modifier = Modifier.weight(1f).aspectRatio(1f)
                        .clickable { onDayClick(day) }.padding(2.dp),
                        contentAlignment = Alignment.TopCenter) {
                        Column(horizontalAlignment = Alignment.CenterHorizontally) {
                            Box(modifier = if (isToday) Modifier.size(28.dp).clip(CircleShape)
                                .background(MaterialTheme.colorScheme.primary) else Modifier.size(28.dp),
                                contentAlignment = Alignment.Center) {
                                Text(day.dayOfMonth.toString(),
                                    style = MaterialTheme.typography.bodySmall,
                                    color = when {
                                        isToday -> MaterialTheme.colorScheme.onPrimary
                                        !isCurrentMonth -> MaterialTheme.colorScheme.onSurfaceVariant.copy(alpha = 0.4f)
                                        else -> MaterialTheme.colorScheme.onSurface
                                    })
                            }
                            if (dayEvents.isNotEmpty()) {
                                Row(horizontalArrangement = Arrangement.spacedBy(2.dp)) {
                                    dayEvents.take(2).forEach { _ ->
                                        Box(modifier = Modifier.size(4.dp).clip(CircleShape)
                                            .background(MaterialTheme.colorScheme.primary))
                                    }
                                }
                                if (dayEvents.size > 2) {
                                    Text("+${dayEvents.size - 2}",
                                        style = MaterialTheme.typography.labelSmall,
                                        color = MaterialTheme.colorScheme.primary)
                                }
                            }
                        }
                    }
                }
            }
        }
    }
}
