namespace FaunaApp.Core.Calendar;

/// <summary>
/// Arbitrates a month-grid day cell's two ratified gestures against each other:
/// a <b>single</b> click drills into Day view for that date, a <b>double</b>
/// click opens the new-event compose prefilled with it (docs/goal/ui/events.md
/// § Layout &amp; flow — the Outlook-modeled `events-day-cell-{date}` rows).
///
/// <para>
/// The two are inherently ambiguous: the OS only tells you a click was the
/// first half of a double <i>after</i> the second one arrives. Acting on the
/// first click immediately is what made the double-click gesture
/// <b>unreachable for a real user</b> on this app — `SelectDay` collapses
/// `MonthGrid` synchronously, so the second physical click of a double lands on
/// whatever the Day timeline just put at those screen coordinates (an empty
/// time slot, which quick-created at ITS time — the wrong date entirely). The
/// e2e never saw it because the double-click assertion was `skip_unbuilt` on
/// windows until 2026-08-06.
/// </para>
///
/// <para>
/// The fix is the shape apple already ships and the only one where a real
/// user's double-click reaches the compose: <b>defer the drill-in until the
/// double-click window resolves</b>. SwiftUI does this for FaunaKit's
/// `MonthGridView` implicitly by ordering `.onTapGesture(count: 2)` ahead of
/// `count: 1`; WinUI has no such arbitration, so it is spelled out here. Only
/// one of the two actions ever fires, which is what § Layout &amp; flow
/// describes — the page's older "the drill runs first and the compose opens
/// over it" comment described the collision, not a ratified behavior.
/// </para>
///
/// <para>
/// UI-free and clock-injected on purpose: the caller owns the timer, so the
/// decision logic itself is pinned by tier_1 tests with a fake clock rather
/// than by a test racing a wall clock (e2e-conventions.md point 14 — "cadence
/// logic tiered down to tier_1 mock-clock tests"). The automation path is
/// unaffected: `Actions.DoubleClick` sends a genuine physical double click, so
/// the e2e exercises the same journey a user does.
/// </para>
/// </summary>
public sealed class DayCellClickArbiter
{
    private readonly TimeSpan _window;

    private DateTimeOffset? _pendingDrillIn;
    private DateTimeOffset? _consumedDate;
    private DateTime? _consumedAt;

    /// <param name="window">
    /// The double-click window — on windows the user's own
    /// <c>GetDoubleClickTime()</c>, not a hard-coded constant.
    /// </param>
    public DayCellClickArbiter(TimeSpan window) => _window = window;

    /// <summary>
    /// A click landed on the day cell for <paramref name="date"/>. Returns
    /// <c>true</c> when the caller should (re)arm the drill-in timer for
    /// <see cref="_window"/>, <c>false</c> when this click is the trailing half
    /// of a double click that has already opened the compose.
    /// </summary>
    /// <remarks>
    /// The trailing-half suppression is what makes the arbiter independent of
    /// WinUI's event ordering. A Button raises <c>Click</c> on each release AND
    /// <c>DoubleTapped</c> on the second tap, and the two orders are not
    /// contractual: if <c>DoubleTapped</c> lands first, the second
    /// <c>Click</c> would otherwise re-arm the timer and drill in half a second
    /// after the compose opened.
    /// </remarks>
    public bool OnClick(DateTimeOffset date, DateTime now)
    {
        if (_consumedDate == date && _consumedAt is { } at && now - at <= _window)
        {
            return false;
        }
        _pendingDrillIn = date;
        return true;
    }

    /// <summary>
    /// The drill-in timer elapsed. Returns the date to drill into, or
    /// <c>null</c> when a double click cancelled it first.
    /// </summary>
    public DateTimeOffset? OnDrillInDue()
    {
        var pending = _pendingDrillIn;
        _pendingDrillIn = null;
        return pending;
    }

    /// <summary>
    /// A double click landed on the day cell for <paramref name="date"/>:
    /// cancel any pending drill-in and report the date the compose opens on.
    /// </summary>
    public DateTimeOffset OnDoubleClick(DateTimeOffset date, DateTime now)
    {
        _pendingDrillIn = null;
        _consumedDate = date;
        _consumedAt = now;
        return date;
    }
}
