using FaunaApp.Core.Calendar;
using FaunaApp.Core.Models;
using Xunit;

namespace FaunaApp.Tests;

public class TimeGridLayoutTests
{
    private static EventInfo Ev(string id, DateTimeOffset start, DateTimeOffset end, bool allDay = false) =>
        new(id, id, start, end, null, null, "cal", "", null, allDay);

    // ── Empty-slot quick-create markers (events.md § Week & day timeline views) ──
    //
    // The 96 per-15-min slots the day column tiles UNDER its event blocks, each
    // carrying the indexed ui.yaml id `events-time-slot-{HH-MM}`. Same tiling as
    // linux's reference leg (`week_grid.rs::add_time_slot_markers`), and pinned
    // here for the same reason linux pins it at `week_grid.rs:597-610`: the id
    // format is a cross-app driver contract, so it gets a test that does not
    // need a XAML runtime.

    [Fact]
    public void QuarterHourSlots_TileTheFull24hColumn()
    {
        var slots = TimeGridLayout.QuarterHourSlots();

        Assert.Equal(96, slots.Count);
        Assert.Equal("events-time-slot-00-00", slots[0].ElementId);
        Assert.Equal("events-time-slot-09-15", slots[37].ElementId);
        Assert.Equal("events-time-slot-23-45", slots[95].ElementId);
    }

    [Fact]
    public void QuarterHourSlots_CarryStartMinutesAndAHumanLabel()
    {
        var slots = TimeGridLayout.QuarterHourSlots();

        // StartMinutes drives the marker's vertical placement; the label is the
        // AutomationProperties.Name every slot needs or WinUI 3 marks the peer
        // raw and FlaUI's FindAllDescendants prunes it (the bare-layout trap).
        Assert.Equal(0, slots[0].StartMinutes);
        Assert.Equal(555, slots[37].StartMinutes);   // 09:15
        Assert.Equal(1425, slots[95].StartMinutes);  // 23:45
        Assert.Equal("13:30", slots[54].Label);
    }

    [Fact]
    public void SplitsAllDayFromTimed()
    {
        var day = new DateOnly(2026, 3, 22);
        var d = new DateTimeOffset(2026, 3, 22, 0, 0, 0, TimeSpan.Zero);
        var layout = TimeGridLayout.ForDay(day, new[]
        {
            Ev("allday", d, d.AddDays(1), allDay: true),
            Ev("timed", d.AddHours(9), d.AddHours(10)),
        });
        Assert.Single(layout.AllDay);
        Assert.Single(layout.Timed);
        Assert.Equal("timed", layout.Timed[0].Event.Id);
        Assert.Equal(540, layout.Timed[0].StartMinutes);   // 09:00
        Assert.Equal(60, layout.Timed[0].DurationMinutes);
    }

    [Fact]
    public void OverlappingTimedEventsGetTwoColumns()
    {
        var day = new DateOnly(2026, 3, 22);
        var d = new DateTimeOffset(2026, 3, 22, 0, 0, 0, TimeSpan.Zero);
        var layout = TimeGridLayout.ForDay(day, new[]
        {
            Ev("a", d.AddHours(9), d.AddHours(11)),
            Ev("b", d.AddHours(10), d.AddHours(12)),
        });
        Assert.All(layout.Timed, b => Assert.Equal(2, b.ColumnCount));
        Assert.NotEqual(layout.Timed[0].ColumnIndex, layout.Timed[1].ColumnIndex);
    }

    [Fact]
    public void ExcludesEventsOnOtherDays()
    {
        var day = new DateOnly(2026, 3, 22);
        var other = new DateTimeOffset(2026, 3, 23, 9, 0, 0, TimeSpan.Zero);
        var layout = TimeGridLayout.ForDay(day, new[] { Ev("x", other, other.AddHours(1)) });
        Assert.Empty(layout.Timed);
        Assert.Empty(layout.AllDay);
    }
}
