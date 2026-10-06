import Foundation

public extension Calendar {
    /// Start of the calendar week containing `date` (locale-aware
    /// `.yearForWeekOfYear`/`.weekOfYear` boundary). Shared by the
    /// macOS/iOS week-view navigation chrome (`MacWeekView`/`WeekView`).
    func startOfWeek(for date: Date) -> Date {
        let components = dateComponents([.yearForWeekOfYear, .weekOfYear], from: date)
        return self.date(from: components) ?? date
    }
}

/// Week-view navigation chrome shared by `MacWeekView` (macOS) and `WeekView`
/// (iOS): the 7-day span and "MMM d – MMM d" range label for a given week
/// start (priority #2 — one shared shape instead of two identical copies).
public enum WeekFormat {
    public static func days(from weekStart: Date) -> [Date] {
        (0..<7).compactMap { Calendar.current.date(byAdding: .day, value: $0, to: weekStart) }
    }

    public static func label(from weekStart: Date) -> String {
        let end = Calendar.current.date(byAdding: .day, value: 6, to: weekStart) ?? weekStart
        let fmt = DateFormatter()
        fmt.dateFormat = "MMM d"
        return "\(fmt.string(from: weekStart)) – \(fmt.string(from: end))"
    }
}
