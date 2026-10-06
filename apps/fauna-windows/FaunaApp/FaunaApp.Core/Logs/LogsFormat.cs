using System;
using System.Collections.Generic;
using System.Linq;
using uniffi.fauna_ffi;
using uniffi.fauna_log;

namespace FaunaApp.Core.Logs;

/// <summary>
/// Thin Windows adapter over the shared Logs presentation logic in
/// <c>fauna_log::format</c> (the severity filter ↔ level map, the time/line/row
/// form, the newest-first copy payload). The bug-prone parts no longer live here —
/// they live once in shared Rust and every app renders through them (Linux
/// natively, the native apps over the <c>FaunaFfiMethods.Log*</c> UniFFI
/// exports, the web over WASM; <c>observability.md</c> § Surfaces, priority #2/#3).
/// This class only does the genuinely platform-specific bit — capture the
/// machine's <b>current local UTC offset</b> (so the FFI can render local
/// <c>HH:mm:ss</c> from the entry's epoch-millis without a timezone library on
/// either side) and shuttle the lists across the boundary.
///
/// <para><b>Redaction</b> (<c>observability.md</c> § Persistence &amp; privacy):
/// this layer renders whatever <c>tracing</c> captured; call sites are forbidden
/// from logging message plaintext or secrets. Upheld at the call sites, not here.</para>
/// </summary>
internal static class LogsFormat
{
    /// <summary>Map a severity-filter index to its threshold (0/"All"/out-of-range
    /// ⇒ <c>null</c>) — <c>fauna_log::format::level_for_index</c>.</summary>
    public static LogLevel? LevelForIndex(int index)
        => FaunaFfiMethods.LogLevelForIndex((uint)index);

    /// <summary>Entries at or above <paramref name="min"/> in severity, order
    /// preserved (<c>null</c> ⇒ all) — the in-memory filter for a fetched source
    /// (the admin view's list). <c>fauna_log::format::filter_entries</c>.</summary>
    public static IReadOnlyList<LogEntry> FilterEntries(IReadOnlyList<LogEntry> entries, LogLevel? min)
        => FaunaFfiMethods.LogFilterEntries(entries.ToArray(), min);

    /// <summary><c>LEVEL · HH:mm:ss · target · message</c> — the one-line copy /
    /// <c>log-entry</c> marker form. <c>fauna_log::format::format_line</c>.</summary>
    public static string FormatLine(LogEntry entry)
        => FaunaFfiMethods.LogFormatLine(entry, Services.DeviceOffset.UtcOffsetSeconds());

    /// <summary>The entries joined <b>newest-first</b> into one block — the copy
    /// payload (input oldest-first). <c>fauna_log::format::rendered_text</c>.</summary>
    public static string RenderedText(IReadOnlyList<LogEntry> entries)
        => FaunaFfiMethods.LogRenderedText(entries.ToArray(), Services.DeviceOffset.UtcOffsetSeconds());

    /// <summary>The rendered <c>log-entry</c> rows, <b>newest-first</b> — mapped
    /// from the shared <c>fauna_log::format::rows</c> shape onto the XAML-bound
    /// <see cref="LogRow"/> record so the DataTemplate never sees UniFFI types.</summary>
    public static IReadOnlyList<LogRow> Rows(IReadOnlyList<LogEntry> entries)
        => FaunaFfiMethods.LogRows(entries.ToArray(), Services.DeviceOffset.UtcOffsetSeconds())
            .Select(r => new LogRow(r.line, r.message, r.subtitle))
            .ToList();
}
