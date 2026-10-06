import SwiftUI

/// Shared Outlook-style **month grid**, used by both apple apps (priority #2):
/// a 6×7 day-cell grid for the month containing `month`, modelled on Microsoft
/// Outlook's day-cell interaction (events.md § Layout & flow). Each cell carries
/// the indexed id `events-day-cell-{YYYY-MM-DD}`; a **single click** drills into
/// Day view for that date (`onSelectDay`), a **double-click** on an empty cell
/// opens the new-event compose prefilled with that date (`onNewEventOnDay`).
/// Event titles inside a cell are their own tap targets (`onSelectEvent` →
/// `event_detail`), so a click on an event is not a day-cell click.
///
/// Month-grid date math stays on Swift `Calendar`/`DateComponents` (events.md
/// § Where logic lives — only `find_overlaps` is shared to natives).
public struct MonthGridView: View {
    /// Any date within the month to display.
    let month: Date
    let events: [EventSummary]
    let onSelectDay: (Date) -> Void
    let onNewEventOnDay: (Date) -> Void
    let onSelectEvent: (EventSummary) -> Void

    public init(month: Date, events: [EventSummary],
                onSelectDay: @escaping (Date) -> Void,
                onNewEventOnDay: @escaping (Date) -> Void,
                onSelectEvent: @escaping (EventSummary) -> Void) {
        self.month = month
        self.events = events
        self.onSelectDay = onSelectDay
        self.onNewEventOnDay = onNewEventOnDay
        self.onSelectEvent = onSelectEvent
    }

    private let calendar = Calendar.current

    public var body: some View {
        VStack(spacing: 0) {
            weekdayHeader
            Divider()
            // Eager rows (NOT a LazyVGrid) so every day cell registers its id with
            // the in-process driver regardless of scroll position.
            ForEach(weeks, id: \.self) { week in
                HStack(spacing: 0) {
                    ForEach(week, id: \.self) { day in
                        dayCell(day)
                    }
                }
            }
            Spacer(minLength: 0)
        }
        .accessibilityIdentifier(Ids.eventsMonthGrid)
        .automationValue(Ids.eventsMonthGrid, text: { "" })
        // Keep both the grid container id AND every day-cell id queryable.
        .accessibilityElement(children: .contain)
    }

    // MARK: - Header

    private var weekdayHeader: some View {
        HStack(spacing: 0) {
            ForEach(orderedWeekdaySymbols, id: \.self) { sym in
                Text(sym)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity)
            }
        }
        .padding(.vertical, 4)
    }

    // MARK: - Day cell

    private func dayCell(_ day: Date) -> some View {
        let inMonth = calendar.isDate(day, equalTo: month, toGranularity: .month)
        let iso = Self.isoFormatter.string(from: day)
        let dayEvents = CalendarLayout.eventsForDay(events, day: day, calendar: calendar)
        return VStack(alignment: .leading, spacing: 2) {
            Text("\(calendar.component(.day, from: day))")
                .font(.caption)
                .fontWeight(calendar.isDateInToday(day) ? .bold : .regular)
                .foregroundStyle(calendar.isDateInToday(day) ? Color.accentColor
                                 : (inMonth ? .primary : .secondary))
                .frame(maxWidth: .infinity, alignment: .trailing)

            // Up to two event titles (their own tap targets → event_detail).
            ForEach(dayEvents.prefix(2)) { event in
                Button { onSelectEvent(event) } label: {
                    Text(event.summary)
                        .font(.system(size: 9))
                        .lineLimit(1)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.horizontal, 2)
                        .background(Color.accentColor.opacity(0.18))
                        .foregroundStyle(Color.accentColor)
                        .clipShape(RoundedRectangle(cornerRadius: 2))
                }
                .buttonStyle(.plain)
            }
            if dayEvents.count > 2 {
                Text("+\(dayEvents.count - 2)")
                    .font(.system(size: 9))
                    .foregroundStyle(.secondary)
                    .padding(.leading, 2)
            }
            Spacer(minLength: 0)
        }
        .padding(3)
        .frame(maxWidth: .infinity, minHeight: 64, alignment: .topLeading)
        .background(calendar.isDateInToday(day) ? Color.accentColor.opacity(0.06) : .clear)
        .overlay(Rectangle().stroke(Color(white: 0.5).opacity(0.18), lineWidth: 0.5))
        .opacity(inMonth ? 1 : 0.5)
        .contentShape(Rectangle())
        // Real-user gestures: double-tap an empty cell → new event; single → day.
        .onTapGesture(count: 2) { onNewEventOnDay(day) }
        .onTapGesture(count: 1) { onSelectDay(day) }
        .accessibilityIdentifier("events-day-cell-\(iso)")
        .automationActivate(
            "events-day-cell-\(iso)",
            doubleActivate: { onNewEventOnDay(day) }
        ) { onSelectDay(day) }
    }

    // MARK: - Grid math (Swift Calendar)

    /// The 6 weeks (rows) of 7 days spanning the month, including the leading /
    /// trailing days of adjacent months (Outlook fills the full grid).
    private var weeks: [[Date]] {
        let comps = calendar.dateComponents([.year, .month], from: month)
        guard let firstOfMonth = calendar.date(from: comps) else { return [] }
        let firstWeekday = calendar.component(.weekday, from: firstOfMonth)
        let leading = (firstWeekday - calendar.firstWeekday + 7) % 7
        guard let gridStart = calendar.date(byAdding: .day, value: -leading, to: firstOfMonth) else { return [] }
        let cells = (0..<42).compactMap { calendar.date(byAdding: .day, value: $0, to: gridStart) }
        return stride(from: 0, to: cells.count, by: 7).map { Array(cells[$0..<min($0 + 7, cells.count)]) }
    }

    /// Locale short weekday symbols rotated to start at `calendar.firstWeekday`.
    private var orderedWeekdaySymbols: [String] {
        let syms = calendar.shortWeekdaySymbols // index 0 = Sunday
        let start = calendar.firstWeekday - 1
        return (0..<7).map { syms[(start + $0) % 7] }
    }

    private static let isoFormatter: DateFormatter = {
        let f = DateFormatter()
        f.calendar = Calendar(identifier: .gregorian)
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd"
        return f
    }()
}
