using System;
using System.Collections.Generic;
using System.Linq;
using FaunaApp.Core.Models;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.Calendar;

public record TimedBlock(EventInfo Event, int StartMinutes, int DurationMinutes, int ColumnIndex, int ColumnCount);
public record DayColumnLayout(IReadOnlyList<EventInfo> AllDay, IReadOnlyList<TimedBlock> Timed);

/// <summary>
/// One 15-minute empty-slot quick-create target in a week/day day column
/// (events.md § Week &amp; day timeline views; ui.yaml <c>events-time-slot</c>).
/// </summary>
public record TimeSlot(int Hour, int Minute)
{
    /// <summary>Minutes past midnight — the marker's vertical placement.</summary>
    public int StartMinutes => Hour * 60 + Minute;

    /// <summary>The indexed ui.yaml id, <c>events-time-slot-{HH-MM}</c>.</summary>
    public string ElementId => $"events-time-slot-{Hour:D2}-{Minute:D2}";

    /// <summary>
    /// <c>HH:MM</c> — the marker's <c>AutomationProperties.Name</c>. Required:
    /// without a Name, WinUI 3 marks the peer "raw" and FlaUI's
    /// FindAllDescendants prunes the element (the bare-layout prune trap).
    /// </summary>
    public string Label => $"{Hour:D2}:{Minute:D2}";
}

/// <summary>
/// Computes the all-day / timed split and the timed blocks' minute geometry +
/// overlap columns for one day column of a week/day time grid — all of it now
/// shared Rust (FaunaFfiMethods.DayColumnLayout: classification, minute geometry,
/// AND overlap packing in one call); only the day filter stays .NET date math
/// (events.md § Where logic lives).
/// </summary>
public static class TimeGridLayout
{
    /// <summary>
    /// The 96 quarter-hour empty-slot quick-create targets a day column tiles
    /// UNDER its event blocks, 00:00 → 23:45 (events.md § Week &amp; day timeline
    /// views; the same 24h ÷ 15 min tiling as linux's reference leg
    /// <c>week_grid.rs::add_time_slot_markers</c> and tui's <c>grids.rs</c>).
    ///
    /// Pure and view-free so the id format — a cross-app driver contract — is
    /// pinned by a unit test rather than only by an e2e click, the same seam
    /// linux gets from its callback-taking marker builder.
    /// </summary>
    public static IReadOnlyList<TimeSlot> QuarterHourSlots() =>
        Enumerable.Range(0, 24 * 4)
            .Select(i => new TimeSlot(i * 15 / 60, i * 15 % 60))
            .ToList();

    public static DayColumnLayout ForDay(DateOnly day, IEnumerable<EventInfo> events)
    {
        // Use e.Start.DateTime (the stored local-offset component, not LocalDateTime
        // which would apply the machine timezone and shift UTC events across midnight).
        var onDay = events
            .Where(e => DateOnly.FromDateTime(e.Start.DateTime) == day)
            .ToList();
        if (onDay.Count == 0)
            return new DayColumnLayout(Array.Empty<EventInfo>(), Array.Empty<TimedBlock>());

        var placements = FaunaFfiMethods.DayColumnLayout(onDay.Select(e => new FfiDayEvent(
            e.Start.ToString("yyyy-MM-ddTHH:mm:ss"),
            e.End.ToString("yyyy-MM-ddTHH:mm:ss"))).ToArray());

        var allDay = new List<EventInfo>();
        var timed = new List<TimedBlock>();
        for (int i = 0; i < onDay.Count; i++)
        {
            var p = placements[i];
            if (p.allDay)
            {
                allDay.Add(onDay[i]);
            }
            else
            {
                timed.Add(new TimedBlock(
                    onDay[i],
                    (int)p.startMin,
                    (int)(p.endMin - p.startMin),
                    (int)p.columnIndex,
                    (int)p.totalColumns));
            }
        }
        return new DayColumnLayout(allDay, timed);
    }
}
