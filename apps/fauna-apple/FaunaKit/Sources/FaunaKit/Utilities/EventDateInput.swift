import Foundation

/// Shared date-text helpers for the event-create form's combined `event-dtstart`
/// / `event-dtend` fields. Per `ui.yaml` (`event-dtstart`: a single combined
/// `YYYY-MM-DDTHH:MM` `text_input` on all 7 apps), both apple shells —
/// `EventFormView` (iOS) and `MacEventFormView` (macOS) — drive the same typeable
/// text fields through these helpers, so the parse/prefill rules can never drift
/// between the two apps (priority #2/#4).
public enum EventDateInput {
    /// Normalize the combined-field user input (`"2026-04-01T10:00"`) into a
    /// seconds-bearing RFC 3339 datetime for the API — the shared
    /// `normalizeEventDatetimeInput` (`fauna_core::caltime`, events.md § Where
    /// logic lives), lifted verbatim from this function's own A2 rule (a bare
    /// `YYYY-MM-DDTHH:MM` padded to `YYYY-MM-DDTHH:MM:00`, since a timezone-less
    /// time made the nest's iCalendar serializer emit a malformed compact time no
    /// client's date parser could read — events.md § Implementation status), plus
    /// space-separated date+time unification other apps needed.
    public static func parse(_ input: String) -> String {
        normalizeEventDatetimeInput(input: input)
    }

    /// Format a `Date` as the `YYYY-MM-DDTHH:MM` text the date fields display —
    /// used to seed the compose form from an Outlook day-cell prefill
    /// (`events.md` § Layout & flow).
    public static func prefillString(from date: Date) -> String {
        prefillFormatter.string(from: date)
    }

    private static let prefillFormatter: DateFormatter = {
        let f = DateFormatter()
        f.locale = Locale(identifier: "en_US_POSIX")
        f.dateFormat = "yyyy-MM-dd'T'HH:mm"
        return f
    }()
}
