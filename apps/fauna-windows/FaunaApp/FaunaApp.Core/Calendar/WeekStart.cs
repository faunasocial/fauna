using System;
using System.Globalization;

namespace FaunaApp.Core.Calendar;

/// <summary>
/// Windows' locale week-start probe and the column arithmetic every calendar
/// surface derives from it (<c>docs/goal/ui/events.md</c> § Week &amp; day timeline
/// views: "week-start follows the app locale (each app via its platform
/// mechanism — windows <c>CultureInfo.CurrentCulture.DateTimeFormat.FirstDayOfWeek</c>…)").
///
/// <para><b>Why this exists rather than three call sites each asking
/// `CultureInfo`.</b> Until 2026-08-24 windows disagreed with itself: the week
/// grid probed the culture correctly while the month grid hardcoded Sunday-first
/// — header row and cell placement both — so in a Monday-first locale the two
/// views of the same month disagreed about which column a date sits in. The week
/// range label (added the same week) then made it a THIRD independent probe, in
/// a different assembly. One value, probed once, is what stops the three drifting
/// apart again.</para>
///
/// <para><b>Deliberately .NET's own probe, not shared Rust.</b> `fauna_core`'s
/// <c>locale_week_start</c> is for the two Rust apps, which ship no locale-aware
/// calendar library; § Where logic lives (the 2026-08-01 per-platform amendment)
/// keeps the probe and month-grid math on the platform library for the five apps
/// that have one, and no UniFFI face for it exists. So there is no
/// <c>0 = Monday</c> boundary to cross here: every index below is .NET's
/// <see cref="DayOfWeek"/> (0 = Sunday), start to finish.</para>
/// </summary>
public static class WeekStart
{
    /// <summary>The locale's first day of the week — the one probe.</summary>
    public static DayOfWeek Current => CultureInfo.CurrentCulture.DateTimeFormat.FirstDayOfWeek;

    /// <summary>Which grid column <paramref name="day"/> occupies, given
    /// <paramref name="weekStart"/> — 0 is the leftmost column, whatever weekday
    /// that is. The <c>+ 7</c> before the modulo is load-bearing: C#'s <c>%</c>
    /// keeps the sign of its left operand, so a Monday-first locale rendering a
    /// Sunday would otherwise land on column −1.</summary>
    public static int ColumnOf(DayOfWeek day, DayOfWeek weekStart) =>
        ((int)day - (int)weekStart + 7) % 7;

    /// <summary>Which grid column <paramref name="day"/> occupies under the
    /// current locale.</summary>
    public static int ColumnOf(DayOfWeek day) => ColumnOf(day, Current);

    /// <summary>The weekday rendered in column <paramref name="column"/> — the
    /// inverse of <see cref="ColumnOf(DayOfWeek, DayOfWeek)"/>, for painting a
    /// header row left to right.</summary>
    public static DayOfWeek WeekdayInColumn(int column, DayOfWeek weekStart) =>
        (DayOfWeek)(((int)weekStart + column) % 7);

    /// <summary>The weekday rendered in column <paramref name="column"/> under
    /// the current locale.</summary>
    public static DayOfWeek WeekdayInColumn(int column) => WeekdayInColumn(column, Current);

    /// <summary>The first date of the week containing <paramref name="anchor"/>,
    /// under <paramref name="weekStart"/> — the week grid's leftmost column and
    /// the week label's span start, from one rule.</summary>
    public static DateTimeOffset StartOfWeek(DateTimeOffset anchor, DayOfWeek weekStart) =>
        anchor.AddDays(-ColumnOf(anchor.DayOfWeek, weekStart));

    /// <summary>The first date of the week containing <paramref name="anchor"/>
    /// under the current locale.</summary>
    public static DateTimeOffset StartOfWeek(DateTimeOffset anchor) =>
        StartOfWeek(anchor, Current);
}
