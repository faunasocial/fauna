using System;
using System.IO;
using System.Linq;

namespace FaunaApp.Core.Services;

/// <summary>
/// Where a calendar <c>.ics</c> export lands in a directory (events.md § Import /
/// Export): the calendar's own name made safe as a file name, <c>.ics</c>, and a
/// <c> (2)</c>, <c> (3)</c>, … suffix rather than overwriting a file already there — a
/// second export of the same calendar is a new file, the way a browser download is.
/// Mirrors tui's <c>unused_export_path</c>. Pure but for the existence probe.
/// </summary>
internal static class IcsExportPath
{
    private const char Replacement = '_';

    internal static string Unused(string dir, string calendarName)
    {
        var stem = FileStem(calendarName);
        var first = Path.Combine(dir, stem + ".ics");
        if (!File.Exists(first)) return first;
        for (var n = 2; ; n++)
        {
            var next = Path.Combine(dir, $"{stem} ({n}).ics");
            if (!File.Exists(next)) return next;
        }
    }

    /// <summary>A calendar name as a file-name stem: path separators and control or
    /// reserved characters become <c>_</c>, and an empty result falls back to
    /// <c>calendar</c>.</summary>
    internal static string FileStem(string calendarName)
    {
        var safe = new string((calendarName ?? "").Trim()
            .Select(c => char.IsControl(c) || "/\\:*?\"<>|".Contains(c) ? Replacement : c)
            .ToArray());
        safe = safe.Trim('.').Trim();
        return safe.Length == 0 ? "calendar" : safe;
    }
}
