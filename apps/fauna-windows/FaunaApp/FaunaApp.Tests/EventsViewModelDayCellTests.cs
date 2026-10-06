using Xunit;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Tests;

// ── EventsViewModel day-cell drill-in (Outlook month→day) Tests ──
//
// The month grid's `events-day-cell-{YYYY-MM-DD}` single-click switches the
// Events page to Day view for that date (docs/goal/ui/events.md § User actions —
// the `events-day-cell-{date}` single-click row). View-mode + date is client
// glue today (legacy path), consistent with the existing `calendar-view-*`
// toggles; this asserts the VM-side state transition the page calls into.

public class EventsViewModelDayCellTests
{
    [Fact]
    public void SelectDay_SwitchesToDayView_ForThatDate()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventsViewModel(rpc);

        vm.SelectDay(new DateTimeOffset(2026, 3, 16, 0, 0, 0, TimeSpan.Zero));

        // The wire word, not the enum: this file has no `using uniffi.fauna_ffi`,
        // and reading it through the shared vocabulary is what the id contract
        // actually pins.
        Assert.Equal("day", vm.ViewModeWire);
        Assert.Equal(new DateTime(2026, 3, 16), vm.CurrentDay.Date);
    }

    // The end state above is not enough: the page rebuilds the day timeline
    // INSIDE its ViewMode observer and reads CurrentDay while doing it
    // (EventsPage.xaml.cs::BuildDayTimeline), and CommunityToolkit.Mvvm raises
    // PropertyChanged synchronously. So the assignment ORDER inside SelectDay is
    // production behavior, not style: with ViewMode set first, the drill-in
    // built the timeline for the PREVIOUSLY selected day (initially today), and
    // nothing observes CurrentDay to correct it — the view stayed on the wrong
    // day until the ~10s refresh poll rebuilt it. This reproduces that coupling
    // exactly: sample CurrentDay at the instant the ViewMode change is observed.
    //
    // It is also the bug that produced the symptom row 38 was filed for: the
    // second physical click of a month-cell double-click landed on an empty time
    // slot of a timeline built for the WRONG day, quick-creating there.
    [Fact]
    public void SelectDay_SetsTheDate_BeforeViewModeIsObserved()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventsViewModel(rpc);
        DateTimeOffset? dayAsSeenByTheViewModeObserver = null;
        vm.PropertyChanged += (_, e) =>
        {
            if (e.PropertyName == nameof(EventsViewModel.ViewMode))
            {
                dayAsSeenByTheViewModeObserver = vm.CurrentDay;
            }
        };

        vm.SelectDay(new DateTimeOffset(2026, 3, 16, 0, 0, 0, TimeSpan.Zero));

        Assert.NotNull(dayAsSeenByTheViewModeObserver);
        Assert.Equal(new DateTime(2026, 3, 16), dayAsSeenByTheViewModeObserver!.Value.Date);
    }

    // Slice 2 — double-click an (empty) month-grid day cell → open the new-event
    // compose prefilled with that date (docs/goal/ui/events.md § User actions —
    // the `events-day-cell-{date}` double-click row: "client glue prefills the
    // compose event-form dtstart with the cell's date"). The compose start is
    // the cell date at a 09:00 default hour, in the combined YYYY-MM-DDTHH:MM
    // shape EventStartBox (`event-dtstart`) accepts. The page opens the compose
    // panel + fills the field off this VM state (mirrors the SelectDay glue).
    [Fact]
    public void NewEventOnDay_PrefillsComposeStart_WithThatDateAtNineAm()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventsViewModel(rpc);

        vm.NewEventOnDay(new DateTimeOffset(2026, 3, 16, 0, 0, 0, TimeSpan.Zero));

        Assert.Equal("2026-03-16T09:00", vm.ComposeStartPrefill);
    }

    // Empty-time-slot quick-create (docs/goal/ui/events.md § Week & day timeline
    // views: "Clicking an empty time slot opens the new-event compose prefilled
    // at that date/time"). The week/day grids' `events-time-slot-{HH-MM}` markers
    // call this with the slot's OWN snapped time — the month grid's date-only
    // drill-in is the same prefill at a 09:00 default, so NewEventOnDay delegates
    // here rather than formatting a second time.
    [Fact]
    public void NewEventAtSlot_PrefillsComposeStart_WithTheSlotsOwnTime()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventsViewModel(rpc);

        vm.NewEventAtSlot(new DateTimeOffset(2026, 3, 16, 0, 0, 0, TimeSpan.Zero), 9, 15);

        Assert.Equal("2026-03-16T09:15", vm.ComposeStartPrefill);
    }

    [Fact]
    public void NewEventAtSlot_ZeroPadsBothHalvesOfTheTime()
    {
        var rpc = new MockNestRpcClient();
        var vm = new EventsViewModel(rpc);

        // 00:00 is a real slot (the column tiles the full 24h) and is exactly
        // where an unpadded format would emit "2026-03-16T0:0" — a string
        // event-dtstart cannot parse.
        vm.NewEventAtSlot(new DateTimeOffset(2026, 3, 16, 0, 0, 0, TimeSpan.Zero), 0, 0);

        Assert.Equal("2026-03-16T00:00", vm.ComposeStartPrefill);
    }
}
