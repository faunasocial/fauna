import SwiftUI

/// Shared Outlook-style week/day **time grid**, used by both apple apps
/// (priority #2) — the macOS `MacWeekView`/`MacDayTimelineView` and iOS
/// `WeekView`/`DayTimelineView` shells are thin navigation chrome over this one
/// component, so a week column and the day view render with the *same* geometry
/// and the *same* `find_overlaps` packing (events.md § Week & day timeline views).
///
/// It renders, top-to-bottom: a day-column header row (weekday + number, today
/// highlighted), a separate **all-day band** (`calendar-allday-band`, shown only
/// when the range carries all-day events), and a vertically-scrolling timed grid
/// — a left hour gutter (00:00–23:00, half-hour rows) and one positioned-block
/// column per day, with a **current-time line** (`calendar-current-time`) on
/// today's column and an ~08:00 auto-scroll. Timed events are positioned blocks
/// (`calendar-event-block`, indexed, click → `onSelectEvent`); overlapping events
/// pack into side-by-side sub-columns via the shared `find_overlaps`.
///
/// Pass 7 `days` for the week view and 1 for the day view (same renderer, single
/// full-width column — the contract's "the *same* day-column renderer").
public struct WeekDayTimeGrid: View {
    let days: [Date]
    let events: [EventSummary]
    /// `calendar-week-grid` (week) or `calendar-day-timeline` (day).
    let containerId: String
    let onSelectEvent: (EventSummary) -> Void
    /// Outlook empty-slot click → new-event compose prefilled at that **instant**
    /// — the column's date carrying the tapped slot's snapped time, not the day's
    /// midnight. Callers seed the compose verbatim (`EventsVM.beginCompose(atSlot:)`).
    let onEmptySlot: ((Date) -> Void)?

    public init(days: [Date], events: [EventSummary], containerId: String,
                onSelectEvent: @escaping (EventSummary) -> Void,
                onEmptySlot: ((Date) -> Void)? = nil) {
        self.days = days
        self.events = events
        self.containerId = containerId
        self.onSelectEvent = onSelectEvent
        self.onEmptySlot = onEmptySlot
    }

    private let calendar = Calendar.current
    private let hours = Array(0..<24)

    public var body: some View {
        VStack(spacing: 0) {
            dayHeaderRow
            Divider()
            allDayBand
            timedGrid
        }
        .accessibilityIdentifier(containerId)
        .automationValue(containerId, text: { "" })
        // Keep BOTH the container id AND the child block / band / current-time
        // ids queryable by the in-process driver (the bare-container-clobbers-
        // children trap — see the apple gotchas memory).
        .accessibilityElement(children: .contain)
    }

    // MARK: - Day header row

    private var dayHeaderRow: some View {
        HStack(spacing: 0) {
            Color.clear.frame(width: CalendarLayout.gutterWidth)
            ForEach(days, id: \.self) { day in
                VStack(spacing: 2) {
                    Text(weekdayShort(day))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                    Text("\(calendar.component(.day, from: day))")
                        .font(.subheadline)
                        .fontWeight(calendar.isDateInToday(day) ? .bold : .regular)
                        .foregroundStyle(calendar.isDateInToday(day) ? Color.accentColor : .primary)
                }
                .frame(maxWidth: .infinity)
            }
        }
        .padding(.vertical, 4)
    }

    // MARK: - All-day band

    /// All-day events keyed by day, in `days` order. Empty when none.
    private var allDayByDay: [[EventSummary]] {
        days.map { day in
            CalendarLayout.eventsForDay(events, day: day, calendar: calendar).filter {
                eventIsAllDay(start: $0.dtstart, end: $0.dtend)
            }
        }
    }

    @ViewBuilder
    private var allDayBand: some View {
        let perDay = allDayByDay
        if perDay.contains(where: { !$0.isEmpty }) {
            HStack(alignment: .top, spacing: 0) {
                Text(L.events.allDay)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .frame(width: CalendarLayout.gutterWidth, alignment: .trailing)
                    .padding(.trailing, 4)
                ForEach(Array(perDay.enumerated()), id: \.offset) { _, dayEvents in
                    VStack(spacing: 1) {
                        // All-day chips live inside calendar-allday-band (queried by
                        // the band id); calendar-event-block is reserved for the
                        // positioned *timed* blocks in the grid below.
                        ForEach(dayEvents) { event in
                            Button { onSelectEvent(event) } label: {
                                Text(event.summary)
                                    .font(.caption2)
                                    .lineLimit(1)
                                    .frame(maxWidth: .infinity, alignment: .leading)
                                    .padding(.horizontal, 4)
                                    .padding(.vertical, 1)
                                    .background(Color.accentColor.opacity(0.2))
                                    .foregroundStyle(Color.accentColor)
                                    .clipShape(RoundedRectangle(cornerRadius: 3))
                            }
                            .buttonStyle(.plain)
                        }
                        Spacer(minLength: 0)
                    }
                    .frame(maxWidth: .infinity, alignment: .top)
                }
            }
            .padding(.vertical, 2)
            .frame(maxHeight: 60)
            .accessibilityIdentifier(Ids.calendarAlldayBand)
            // The band's text is the summaries it paints, in day order — what
            // every other app's band reads back (web's textContent, tui's row),
            // so "this all-day event sits in the band" is one cross-app assert.
            .automationValue(Ids.calendarAlldayBand, text: {
                perDay.flatMap { $0.map(\.summary) }.joined(separator: " ")
            })
            .accessibilityElement(children: .contain)
            Divider()
        }
    }

    // MARK: - Timed grid

    private var timedGrid: some View {
        ScrollViewReader { proxy in
            ScrollView {
                ZStack(alignment: .topLeading) {
                    // Hour separator lines spanning the full width (gutter + columns).
                    VStack(spacing: 0) {
                        ForEach(hours, id: \.self) { hour in
                            HStack(alignment: .top, spacing: 0) {
                                Text(CalendarLayout.hourLabel(hour))
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                                    .frame(width: CalendarLayout.gutterWidth, alignment: .trailing)
                                    .padding(.trailing, 4)
                                Rectangle()
                                    .fill(Color(white: 0.5).opacity(0.25))
                                    .frame(height: 0.5)
                                    .frame(maxWidth: .infinity)
                            }
                            .frame(height: CalendarLayout.hourPx, alignment: .top)
                            .id(hour)
                        }
                    }

                    // Positioned blocks + current-time line, offset past the gutter.
                    HStack(spacing: 0) {
                        Color.clear.frame(width: CalendarLayout.gutterWidth)
                        ForEach(days, id: \.self) { day in
                            dayColumn(day)
                        }
                    }
                }
            }
            .onAppear { proxy.scrollTo(8, anchor: .top) }
            .onChange(of: days) { proxy.scrollTo(8, anchor: .top) }
        }
    }

    private func dayColumn(_ day: Date) -> some View {
        let layout = CalendarLayout.layoutDay(events: CalendarLayout.eventsForDay(events, day: day, calendar: calendar))
        let isToday = calendar.isDateInToday(day)
        return GeometryReader { geo in
            ZStack(alignment: .topLeading) {
                // Empty-slot tap targets (behind the blocks): Outlook
                // click-to-create, one per 15-minute slot.
                if let onEmptySlot {
                    VStack(spacing: 0) {
                        ForEach(0..<CalendarLayout.slotsPerDay, id: \.self) { slot in
                            timeSlot(day, minutes: slot * 15, onEmptySlot: onEmptySlot)
                        }
                    }
                }

                ForEach(layout.timed) { block in
                    let colW = geo.size.width / CGFloat(block.totalColumns)
                    eventBlock(block.event)
                        .frame(width: max(colW - CalendarLayout.columnGap, 1),
                               height: block.heightPx, alignment: .topLeading)
                        .offset(x: CGFloat(block.columnIndex) * colW, y: block.topPx)
                }

                if isToday {
                    Rectangle()
                        .fill(Color.red)
                        .frame(height: 2)
                        .offset(y: currentTimeOffset)
                        .accessibilityIdentifier(Ids.calendarCurrentTime)
                        .automationValue(Ids.calendarCurrentTime, text: { "" })
                }
            }
            .frame(width: geo.size.width, height: CalendarLayout.gridHeight, alignment: .topLeading)
        }
        .frame(maxWidth: .infinity)
        .frame(height: CalendarLayout.gridHeight)
    }

    /// One empty-slot quick-create target — `events-time-slot-{HH-MM}`, the
    /// Outlook click-to-create affordance (events.md § Week & day timeline
    /// views), matching linux's 96 per-column markers.
    ///
    /// The slot's time is **closure-captured, never derived from the tap
    /// location**: headless automation drivers activate an element without ever
    /// emitting a pointer position (they would report (0,0)), so a location-based
    /// grid would quick-create at midnight under every test — the same reason
    /// linux tiles real marker widgets instead of hit-testing one background
    /// gesture (`week_grid.rs::add_time_slot_markers`).
    ///
    /// Stacked BEFORE the event blocks in the enclosing `ZStack`, so a tap
    /// landing on a block hits the block's button rather than the slot beneath
    /// it — the block-vs-slot precedence is structural (paint order), not
    /// gesture-priority bookkeeping, again as on linux.
    private func timeSlot(_ day: Date, minutes: Int,
                          onEmptySlot: @escaping (Date) -> Void) -> some View {
        let id = String(format: "events-time-slot-%02d-%02d", minutes / 60, minutes % 60)
        let start = slotStart(day, minutes: minutes)
        return Color.clear
            .frame(height: CalendarLayout.quarterHourPx)
            .contentShape(Rectangle())
            .onTapGesture { onEmptySlot(start) }
            .accessibilityIdentifier(id)
            .automationActivate(id, text: { "" }) { onEmptySlot(start) }
    }

    /// The instant a slot `minutes` after midnight on `day` represents.
    ///
    /// Set on the wall clock rather than added to midnight so the prefilled time
    /// always reads back as the `HH:MM` in the slot's own id — on a DST
    /// spring-forward day, adding 9h15m to midnight would land on 10:15. Falls
    /// back to the additive form for the hour DST deletes (which `bySettingHour`
    /// cannot name), and to the day itself if even that fails.
    private func slotStart(_ day: Date, minutes: Int) -> Date {
        let midnight = calendar.startOfDay(for: day)
        return calendar.date(bySettingHour: minutes / 60, minute: minutes % 60,
                             second: 0, of: midnight)
            ?? calendar.date(byAdding: .minute, value: minutes, to: midnight)
            ?? midnight
    }

    private func eventBlock(_ event: EventSummary) -> some View {
        Button { onSelectEvent(event) } label: {
            Text(event.summary)
                .font(.caption2)
                .lineLimit(2)
                .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .topLeading)
                .padding(3)
                .background(Color.accentColor.opacity(0.2))
                .foregroundStyle(Color.accentColor)
                .clipShape(RoundedRectangle(cornerRadius: 4))
        }
        .buttonStyle(.plain)
        .accessibilityIdentifier(Ids.calendarEventBlock)
        // `text` = the summary the block renders, `value` = the event id
        // (events.md § Week & day timeline views: blocks are indexed "by event
        // id"). The text read is the CROSS-APP identity a test matches on —
        // web's block spans, windows' `AutomationProperties.Name`, tui's label
        // and android's Card all read back the summary, so apple must too or an
        // identity assert has to be written per-app (priority #1).
        .automationActivate(Ids.calendarEventBlock,
                            text: { event.summary },
                            value: { event.id }) {
            onSelectEvent(event)
        }
    }

    // MARK: - Helpers

    private var currentTimeOffset: CGFloat {
        let mins = CalendarLayout.minutesSinceMidnight(Date(), calendar)
        return CGFloat(mins) / 30.0 * CalendarLayout.halfHourPx
    }

    private func weekdayShort(_ date: Date) -> String {
        let fmt = DateFormatter()
        fmt.dateFormat = "EEE"
        return fmt.string(from: date)
    }
}
