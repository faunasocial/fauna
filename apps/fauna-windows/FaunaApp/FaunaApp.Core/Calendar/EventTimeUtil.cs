using System;
using System.Globalization;

namespace FaunaApp.Core.Calendar;

/// <summary>
/// .NET-side calendar date helpers — the platform date-library half of
/// events.md § Where logic lives (the natives own date math; all-day
/// classification + time-grid geometry are shared Rust —
/// <see cref="uniffi.fauna_ffi.FaunaFfiMethods.EventIsAllDay"/> /
/// <see cref="uniffi.fauna_ffi.FaunaFfiMethods.DayColumnLayout"/>).
/// </summary>
public static class EventTimeUtil
{
    // The basic/compact ISO-8601 forms the general DateTimeOffset.TryParse
    // rejects — what build_vevent + the shared CalDAV path (and the linux Events
    // form) emit, e.g. "20260626T154300Z". A 'Z' suffix is UTC; bare is local.
    private static readonly string[] _basicUtcFormats =
        { "yyyyMMdd'T'HHmmss'Z'", "yyyyMMdd'T'HHmm'Z'" };

    private static readonly string[] _basicLocalFormats =
        { "yyyyMMdd'T'HHmmss", "yyyyMMdd'T'HHmm" };

    /// <summary>
    /// Parse a combined date+time text field (event-dtstart/event-dtend). Empty
    /// text yields <paramref name="fallback"/>. Accepts both <b>extended</b>
    /// ISO-8601 ("2026-06-05T14:30", "2026-06-26T15:43:00Z") via the general
    /// parser and the <b>basic/compact</b> ISO form ("20260626T154300Z") that
    /// build_vevent + the shared CalDAV path emit — a 'Z' suffix is UTC, a bare
    /// timestamp is local. Returns false only when the field is non-empty and
    /// matches none of these. This is the windows half of events.md § Where
    /// logic lives; the accepted-format set is unified with the other apps
    /// (priority #4 — resolve drift, don't fork).
    /// </summary>
    public static bool TryParseEventDateTime(string? text, DateTimeOffset fallback, out DateTimeOffset result)
    {
        if (string.IsNullOrWhiteSpace(text))
        {
            result = fallback;
            return true;
        }
        var trimmed = text.Trim();
        // Extended ISO + anything the general parser handles, first.
        if (DateTimeOffset.TryParse(trimmed, CultureInfo.InvariantCulture, DateTimeStyles.AssumeLocal, out result))
            return true;
        // Basic/compact ISO fallback. Z-suffixed → UTC; bare → local.
        if (DateTimeOffset.TryParseExact(trimmed, _basicUtcFormats, CultureInfo.InvariantCulture,
                DateTimeStyles.AssumeUniversal, out result))
            return true;
        return DateTimeOffset.TryParseExact(trimmed, _basicLocalFormats, CultureInfo.InvariantCulture,
            DateTimeStyles.AssumeLocal, out result);
    }
}
