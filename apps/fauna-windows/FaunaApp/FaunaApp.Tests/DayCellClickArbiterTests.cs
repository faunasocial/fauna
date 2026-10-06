using Xunit;
using FaunaApp.Core.Calendar;

namespace FaunaApp.Tests;

// ── Month day-cell single-vs-double click arbitration ──
//
// The month grid's `events-day-cell-{YYYY-MM-DD}` carries two ratified gestures
// (docs/goal/ui/events.md § Layout & flow): single-click drills into Day view,
// double-click opens the new-event compose prefilled with that date. Acting on
// the first click immediately made the second gesture unreachable for a real
// user — the drill-in collapses MonthGrid, so the second physical click of a
// double lands on the Day timeline's empty time slot underneath and
// quick-creates at ITS time. These pin the decision, with an injected clock, so
// no test has to beat a wall clock (e2e-conventions.md point 14).

public class DayCellClickArbiterTests
{
    private static readonly TimeSpan Window = TimeSpan.FromMilliseconds(500);
    private static readonly DateTime T0 = new(2026, 3, 16, 12, 0, 0, DateTimeKind.Utc);
    private static readonly DateTimeOffset Day16 = new(2026, 3, 16, 0, 0, 0, TimeSpan.Zero);
    private static readonly DateTimeOffset Day17 = new(2026, 3, 17, 0, 0, 0, TimeSpan.Zero);

    [Fact]
    public void ALoneClick_ArmsTheTimer_AndDrillsInWhenItElapses()
    {
        var arbiter = new DayCellClickArbiter(Window);

        Assert.True(arbiter.OnClick(Day16, T0));
        Assert.Equal(Day16, arbiter.OnDrillInDue());
    }

    [Fact]
    public void ADoubleClick_CancelsThePendingDrillIn()
    {
        var arbiter = new DayCellClickArbiter(Window);

        arbiter.OnClick(Day16, T0);
        Assert.Equal(Day16, arbiter.OnDoubleClick(Day16, T0.AddMilliseconds(120)));

        // The whole point: the timer may still fire (the caller races Stop()
        // against an already-queued tick), and it must NOT drill in.
        Assert.Null(arbiter.OnDrillInDue());
    }

    [Fact]
    public void TheTrailingClickOfADouble_DoesNotReArmTheTimer()
    {
        var arbiter = new DayCellClickArbiter(Window);

        // WinUI's Click-vs-DoubleTapped order is not contractual. When
        // DoubleTapped lands first, the second tap's own Click arrives after
        // the compose is already open and must be swallowed — otherwise the
        // page drills into Day view half a second later.
        arbiter.OnClick(Day16, T0);
        arbiter.OnDoubleClick(Day16, T0.AddMilliseconds(120));

        Assert.False(arbiter.OnClick(Day16, T0.AddMilliseconds(130)));
        Assert.Null(arbiter.OnDrillInDue());
    }

    [Fact]
    public void AFreshClickOnTheSameCellAfterTheWindow_ArmsAgain()
    {
        var arbiter = new DayCellClickArbiter(Window);

        arbiter.OnClick(Day16, T0);
        arbiter.OnDoubleClick(Day16, T0.AddMilliseconds(120));

        // A deliberate later single click on the same day is a normal drill-in,
        // not more trailing noise from the double.
        var later = T0.AddMilliseconds(120) + Window + TimeSpan.FromMilliseconds(1);
        Assert.True(arbiter.OnClick(Day16, later));
        Assert.Equal(Day16, arbiter.OnDrillInDue());
    }

    [Fact]
    public void AClickOnADifferentCell_IsNeverSuppressedByAnEarlierDouble()
    {
        var arbiter = new DayCellClickArbiter(Window);

        arbiter.OnClick(Day16, T0);
        arbiter.OnDoubleClick(Day16, T0.AddMilliseconds(120));

        // Same instant, different day: the suppression is keyed to the cell the
        // double consumed, so a fast click on its neighbour still drills in.
        Assert.True(arbiter.OnClick(Day17, T0.AddMilliseconds(130)));
        Assert.Equal(Day17, arbiter.OnDrillInDue());
    }

    [Fact]
    public void TheTimerIsIdempotent_ASecondTickDrillsInNothing()
    {
        var arbiter = new DayCellClickArbiter(Window);

        arbiter.OnClick(Day16, T0);
        Assert.Equal(Day16, arbiter.OnDrillInDue());
        Assert.Null(arbiter.OnDrillInDue());
    }

    [Fact]
    public void ARetargetedClickBeforeTheWindowElapses_DrillsIntoTheLastCell()
    {
        var arbiter = new DayCellClickArbiter(Window);

        arbiter.OnClick(Day16, T0);
        Assert.True(arbiter.OnClick(Day17, T0.AddMilliseconds(80)));

        Assert.Equal(Day17, arbiter.OnDrillInDue());
    }
}
