using System.Linq;
using Xunit;
using FaunaApp.Core.Models;
using FaunaApp.Core.ViewModels;

namespace FaunaApp.Tests;

// ── EventsViewModel change-detection poll Tests ──
//
// The Events page runs a ~10s DispatcherTimer (EventsPage) that re-fetches the
// encrypted CalDAV store so an event/calendar created by an EXTERNAL CalDAV
// client surfaces while the user stays on the page (docs/goal/ui/events.md
// § Implementation status today — the "Quick-appearance refresh" follow-on (c),
// "the same poll on the other five apps"). linux does this already
// (apps/fauna-linux/src/app.rs `rows_differ_by_id` + the CalendarsLoaded/
// EventsLoaded change-detection arms); windows mirrors that shape.
//
// The load-bearing invariant these tests pin: a steady-state poll (backend
// data unchanged) is a UI NO-OP — it must NOT clear+refill `Calendars`/`Events`,
// because that would destroy and rebuild every `calendar-item`/`event-card`
// every tick (flicker + a race with an in-progress click). Only a real change
// (an external write) mutates the bound collections.
public class EventsViewModelPollTests
{
    [Fact]
    public async Task RefreshIfChanged_SteadyState_IsNoOp()
    {
        var rpc = new MockNestRpcClient();
        rpc.NextCaldavCalendars = new[]
        {
            MockNestRpcClient.MakeCaldavCalendar("aa", "Work", "#112233"),
        };
        rpc.NextCaldavEvents = new[]
        {
            MockNestRpcClient.MakeCaldavEvent("e1", "Standup", "2026-03-16T09:00:00", "2026-03-16T09:30:00", calendarIdHex: "aa"),
        };
        var vm = new EventsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);

        Assert.Single(vm.Calendars);
        Assert.Single(vm.Events);

        // Count CollectionChanged after the initial load; a steady-state poll
        // must leave both at 0 (no mutation of the bound collections).
        int calendarsChanged = 0;
        int eventsChanged = 0;
        vm.Calendars.CollectionChanged += (_, _) => calendarsChanged++;
        vm.Events.CollectionChanged += (_, _) => eventsChanged++;

        var result = await vm.RefreshIfChangedAsync();

        Assert.False(result.AnyChanged);
        Assert.False(result.CalendarsChanged);
        Assert.False(result.EventsChanged);
        Assert.Equal(0, calendarsChanged);
        Assert.Equal(0, eventsChanged);
    }

    [Fact]
    public async Task RefreshIfChanged_NewExternalEvent_UpdatesEventsCollection()
    {
        var rpc = new MockNestRpcClient();
        rpc.NextCaldavCalendars = new[]
        {
            MockNestRpcClient.MakeCaldavCalendar("aa", "Work", "#112233"),
        };
        rpc.NextCaldavEvents = new[]
        {
            MockNestRpcClient.MakeCaldavEvent("e1", "Standup", "2026-03-16T09:00:00", "2026-03-16T09:30:00", calendarIdHex: "aa"),
        };
        var vm = new EventsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        Assert.Single(vm.Events);

        int eventsChanged = 0;
        vm.Events.CollectionChanged += (_, _) => eventsChanged++;

        // An external CalDAV client PUTs a new event (new id) into the store.
        rpc.NextCaldavEvents = new[]
        {
            MockNestRpcClient.MakeCaldavEvent("e1", "Standup", "2026-03-16T09:00:00", "2026-03-16T09:30:00", calendarIdHex: "aa"),
            MockNestRpcClient.MakeCaldavEvent("e2", "Lunch", "2026-03-16T12:00:00", "2026-03-16T13:00:00", calendarIdHex: "aa"),
        };

        var result = await vm.RefreshIfChangedAsync();

        Assert.True(result.AnyChanged);
        Assert.True(result.EventsChanged);
        Assert.True(eventsChanged > 0);
        Assert.Equal(2, vm.Events.Count);
        Assert.Contains(vm.Events, e => e.Id == "e2" && e.Summary == "Lunch");
    }

    // ── The CalDAV delta-sync backstop (events.md § Implementation status
    // today): a live single-calendar selection consults the shared
    // `query_events_seeded` seam instead of paying for a full unseal+decode
    // every poll tick. NOT exercised by the two tests above — with no
    // SelectedCalendar, ResolveCalendarSelection always answers the union
    // arm regardless of how many calendars exist (fauna-client-caldav's
    // `resolve_calendar_selection`), which is deliberately unaffected (see
    // RefreshIfChangedAsync's own comment on why the union case keeps the
    // full read: windows folds invited events into it with no seam coverage
    // there yet).

    [Fact]
    public async Task RefreshIfChanged_SelectedCalendarUnchanged_SkipsTheFullReadEntirely()
    {
        var rpc = new MockNestRpcClient();
        rpc.NextCaldavCalendars = new[]
        {
            MockNestRpcClient.MakeCaldavCalendar("aa", "Work", "#112233"),
        };
        rpc.NextCaldavEvents = new[]
        {
            MockNestRpcClient.MakeCaldavEvent("e1", "Standup", "2026-03-16T09:00:00", "2026-03-16T09:30:00", calendarIdHex: "aa"),
        };
        var vm = new EventsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.SelectedCalendar = new CalendarInfo("aa", "Work", "#112233", null);
        Assert.Single(vm.Events);

        // The backstop probe says "aa" is unchanged since the seeding read above.
        rpc.CaldavQueryEventsSeededById = new() { ["aa"] = null };
        int fullReadsBefore = rpc.Calls.Count(c => c == "CaldavQueryEvents");

        int eventsChanged = 0;
        vm.Events.CollectionChanged += (_, _) => eventsChanged++;

        var result = await vm.RefreshIfChangedAsync();

        Assert.False(result.EventsChanged);
        Assert.Equal(0, eventsChanged);
        Assert.Single(vm.Events);
        // The whole point of the seam: no full unseal+decode paid for this tick.
        Assert.Equal(fullReadsBefore, rpc.Calls.Count(c => c == "CaldavQueryEvents"));
        Assert.Contains("CaldavQueryEventsSeeded", rpc.Calls);
    }

    [Fact]
    public async Task RefreshIfChanged_SelectedCalendarChanged_UpdatesFromTheSeededRead()
    {
        var rpc = new MockNestRpcClient();
        rpc.NextCaldavCalendars = new[]
        {
            MockNestRpcClient.MakeCaldavCalendar("aa", "Work", "#112233"),
        };
        rpc.NextCaldavEvents = new[]
        {
            MockNestRpcClient.MakeCaldavEvent("e1", "Standup", "2026-03-16T09:00:00", "2026-03-16T09:30:00", calendarIdHex: "aa"),
        };
        var vm = new EventsViewModel(rpc);
        await vm.LoadCommand.ExecuteAsync(null);
        vm.SelectedCalendar = new CalendarInfo("aa", "Work", "#112233", null);
        Assert.Single(vm.Events);

        // The backstop probe says "aa" changed — hands back the fresh full list.
        rpc.CaldavQueryEventsSeededById = new()
        {
            ["aa"] = new[]
            {
                MockNestRpcClient.MakeCaldavEvent("e1", "Standup", "2026-03-16T09:00:00", "2026-03-16T09:30:00", calendarIdHex: "aa"),
                MockNestRpcClient.MakeCaldavEvent("e2", "Lunch", "2026-03-16T12:00:00", "2026-03-16T13:00:00", calendarIdHex: "aa"),
            },
        };

        var result = await vm.RefreshIfChangedAsync();

        Assert.True(result.EventsChanged);
        Assert.Equal(2, vm.Events.Count);
        Assert.Contains(vm.Events, e => e.Id == "e2" && e.Summary == "Lunch");
        Assert.Contains("CaldavQueryEventsSeeded", rpc.Calls);
    }
}
