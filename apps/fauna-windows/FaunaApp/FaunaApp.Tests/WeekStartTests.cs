using System;
using Xunit;
using FaunaApp.Core.Calendar;

namespace FaunaApp.Tests;

/// <summary>
/// The locale week-start column arithmetic every windows calendar surface derives
/// from (<c>docs/goal/ui/events.md</c> § Week &amp; day timeline views — windows'
/// mechanism is <c>CultureInfo.FirstDayOfWeek</c>).
///
/// <para><b>What these pin is the defect that existed until 2026-08-24:</b> the
/// month grid hardcoded Sunday-first — header row and cell placement both — while
/// the week grid probed the culture correctly, so in a Monday-first locale the two
/// views of the same month disagreed about which column a date sits in. The tests
/// therefore run the SAME dates through BOTH week-starts and assert they differ in
/// the way a rotation must, rather than asserting one hardcoded layout.</para>
///
/// <para>The rotation lives in <c>FaunaApp.Core</c> rather than beside
/// <c>BuildMonthGrid</c> for a reason worth keeping: <c>FaunaApp.Tests</c>
/// references only <c>FaunaApp.Core</c>, so logic left in the WinUI page assembly
/// is unreachable from here and could only ever be covered by an e2e run. Moving
/// it is what makes it testable at all.</para>
///
/// <para>Deliberately NOT parametrized into
/// <c>tests/e2e-unified/tests/test_events_locale_week_start.py</c>: that suite is
/// declared tui-only by design (a generic version would be asserting that four
/// apps ignore an environment variable).</para>
/// </summary>
public class WeekStartTests
{
    /// The whole point of a week start: it decides which column a weekday lands in.
    /// Sunday is column 0 in a Sunday-first locale and column 6 in a Monday-first
    /// one — the exact swap the month grid used to get wrong.
    [Theory]
    [InlineData(DayOfWeek.Sunday, DayOfWeek.Sunday, 0)]
    [InlineData(DayOfWeek.Saturday, DayOfWeek.Sunday, 6)]
    [InlineData(DayOfWeek.Monday, DayOfWeek.Monday, 0)]
    [InlineData(DayOfWeek.Sunday, DayOfWeek.Monday, 6)]
    [InlineData(DayOfWeek.Wednesday, DayOfWeek.Monday, 2)]
    [InlineData(DayOfWeek.Wednesday, DayOfWeek.Sunday, 3)]
    public void ColumnOf_PlacesTheWeekdayUnderItsWeekStart(
        DayOfWeek day, DayOfWeek weekStart, int expected)
    {
        Assert.Equal(expected, WeekStart.ColumnOf(day, weekStart));
    }

    /// <summary>C#'s <c>%</c> keeps the sign of its left operand, so a Monday-first
    /// locale rendering a Sunday would land on column −1 without the <c>+ 7</c>.
    /// A negative column is not a wrong cell — it throws when handed to
    /// <c>Grid.SetColumn</c>, so this guards a crash, not a misplacement.</summary>
    [Fact]
    public void ColumnOf_IsNeverNegative_ForAnyWeekdayAndWeekStart()
    {
        foreach (DayOfWeek day in Enum.GetValues<DayOfWeek>())
            foreach (DayOfWeek start in Enum.GetValues<DayOfWeek>())
            {
                var col = WeekStart.ColumnOf(day, start);
                Assert.InRange(col, 0, 6);
            }
    }

    /// <summary>Header and cells must agree, or the grid labels a column with one
    /// weekday and fills it with another — which is exactly what a locale-aware
    /// cell placement under a Sunday-first header would have produced had only
    /// half this leg landed.</summary>
    [Fact]
    public void WeekdayInColumn_IsTheInverseOfColumnOf()
    {
        foreach (DayOfWeek start in Enum.GetValues<DayOfWeek>())
            for (int col = 0; col < 7; col++)
                Assert.Equal(col, WeekStart.ColumnOf(WeekStart.WeekdayInColumn(col, start), start));
    }

    /// <summary>Every column is used exactly once — a rotation, not a mapping that
    /// could collapse two weekdays onto one column.</summary>
    [Fact]
    public void WeekdayInColumn_CoversTheWholeWeek_ForEveryWeekStart()
    {
        foreach (DayOfWeek start in Enum.GetValues<DayOfWeek>())
        {
            var week = new HashSet<DayOfWeek>();
            for (int col = 0; col < 7; col++) week.Add(WeekStart.WeekdayInColumn(col, start));
            Assert.Equal(7, week.Count);
            Assert.Equal(start, WeekStart.WeekdayInColumn(0, start));
        }
    }

    /// <summary>The week grid's leftmost column and the week label's span start
    /// come from this one rule, so they cannot name different weeks.</summary>
    [Theory]
    [InlineData(DayOfWeek.Sunday, "2026-03-15")]   // Tue 2026-03-17 → the Sunday before
    [InlineData(DayOfWeek.Monday, "2026-03-16")]   // …or the Monday before
    public void StartOfWeek_SnapsBackToTheWeekStart(DayOfWeek weekStart, string expected)
    {
        var tuesday = new DateTimeOffset(2026, 3, 17, 0, 0, 0, TimeSpan.Zero);

        var start = WeekStart.StartOfWeek(tuesday, weekStart);

        Assert.Equal(expected, start.ToString("yyyy-MM-dd"));
        Assert.Equal(weekStart, start.DayOfWeek);
        // Never forward: the anchor's own week, not the next one.
        Assert.True(start <= tuesday);
        Assert.True((tuesday - start).TotalDays < 7);
    }

    /// <summary>A date that IS the week start stays put — the off-by-one a
    /// snap-back is most likely to get wrong.</summary>
    [Fact]
    public void StartOfWeek_LeavesTheWeekStartItselfAlone()
    {
        var monday = new DateTimeOffset(2026, 3, 16, 0, 0, 0, TimeSpan.Zero);

        Assert.Equal(monday, WeekStart.StartOfWeek(monday, DayOfWeek.Monday));
        // …and under a Sunday-first locale the same Monday snaps back one day.
        Assert.Equal("2026-03-15",
            WeekStart.StartOfWeek(monday, DayOfWeek.Sunday).ToString("yyyy-MM-dd"));
    }
}
