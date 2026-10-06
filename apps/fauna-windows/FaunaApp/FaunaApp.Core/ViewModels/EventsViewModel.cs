using System.Collections.ObjectModel;
using System.Linq;
using CommunityToolkit.Mvvm.ComponentModel;
using CommunityToolkit.Mvvm.Input;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using uniffi.fauna_ffi;

namespace FaunaApp.Core.ViewModels;

/// <summary>
/// Fields the events page collects for a new event (the create form's inputs).
/// The calendar + uid are supplied by the view model from
/// <see cref="EventsViewModel.SelectedCalendar"/>, not the form.
/// </summary>
public record EventDraft(string Summary, string Dtstart, string Dtend, string? Description, string? Location);

/// <summary>
/// One `CalendarList` sidebar row: the underlying <see cref="CalendarInfo"/> (the
/// `calendar-item` selection target, unchanged) plus the `calendar-visibility`
/// display-filter checkbox's bound, mutable checked state (events.md § Where
/// logic lives → *Which calendars display*). Two independent affordances per
/// row — selecting narrows the agenda to just this calendar; the checkbox
/// toggles membership in the no-selection union's filter — never one control.
/// </summary>
public sealed partial class CalendarRowVm : ObservableObject
{
    public CalendarInfo Calendar { get; init; } = null!;
    [ObservableProperty] private bool _isVisible;
}

public partial class EventsViewModel : ViewModelBase
{
    [ObservableProperty] private bool _isLoading;
    [ObservableProperty] private DateTimeOffset _currentMonth = DateTimeOffset.Now;
    [ObservableProperty] private DateTimeOffset _currentDay = DateTimeOffset.Now;

    /// The active view, in the SHARED vocabulary
    /// (<c>fauna_core::caltime::CalendarViewMode</c> over UniFFI) rather than the
    /// bare <c>"agenda"|"month"|"week"|"day"</c> string this used to hold. The
    /// string was not merely untyped: it is what let the pan control step a
    /// whole month in every mode, because nothing tied the word to a policy.
    ///
    /// <para>Hand-written rather than <c>[ObservableProperty]</c>, and
    /// <c>internal</c>, because the generated type is UniFFI-<c>internal</c> and
    /// the generator emits a <c>public</c> property — which a public class cannot
    /// carry for an internal type. The WinUI and test assemblies both see it via
    /// <c>[InternalsVisibleTo]</c>, the same route <c>BackupsViewModel.KindOptions</c>
    /// takes.</para>
    ///
    /// <para><c>CalendarViewMode::from_wire</c> was deliberately NOT minted for
    /// this leg: nothing here parses a string into a mode — every write is a
    /// literal and every read an equality compare — so holding the enum deletes
    /// the strings outright rather than needing a parser
    /// (<c>events.md</c> § Implementation status today, the apple cell).</para>
    private FfiCalendarViewMode _viewMode = FfiCalendarViewMode.Agenda;

    internal FfiCalendarViewMode ViewMode
    {
        get => _viewMode;
        set
        {
            if (_viewMode == value) return;
            _viewMode = value;
            OnPropertyChanged(nameof(ViewMode));
            OnPropertyChanged(nameof(MonthYearDisplay));
        }
    }

    /// <summary>The active view's WIRE word — the one spelling the automation
    /// surface and any persisted view state use, read from the shared
    /// <c>calendar_view_mode_wire</c> rather than a C# switch that could drift
    /// from it.</summary>
    internal string ViewModeWire => FaunaFfiMethods.CalendarViewModeWire(_viewMode);

    /// The combined date+time string the page prefills into the new-event
    /// compose's start field (`event-dtstart`) when a month-grid day cell is
    /// double-clicked (Outlook's double-click-empty-day → new event). The page
    /// observes this and opens the compose panel; see <see cref="NewEventOnDay"/>.
    [ObservableProperty] private string? _composeStartPrefill;

    /// The calendar new events are created in + (for the calendar query branch)
    /// the events list is scoped to. Set by the page's calendar-list selection
    /// (mirrors macOS `EventsVM.selectedCalendar`).
    [ObservableProperty] private CalendarInfo? _selectedCalendar;

    public ObservableCollection<CalendarInfo> Calendars { get; } = new();
    public ObservableCollection<EventInfo> Events { get; } = new();

    /// <summary>
    /// One <see cref="CalendarRowVm"/> per <see cref="Calendars"/> entry, rebuilt
    /// whenever <see cref="Calendars"/> is replaced — the page's `CalendarList`
    /// binds to this (not <see cref="Calendars"/> directly) so each row carries
    /// its own bindable `calendar-visibility` checked state.
    /// </summary>
    public ObservableCollection<CalendarRowVm> CalendarRows { get; } = new();

    /// <summary>
    /// The `calendar-visibility` display filter's checked set (events.md § Where
    /// logic lives → *Which calendars display*) — client-side only, never
    /// persisted server-side. An **empty** set means "no filter" (the full
    /// union); <see cref="SeedVisibleCalendarIds"/> fills it on every path that
    /// replaces <see cref="Calendars"/>, so a fresh page paints every box
    /// checked rather than leaning on that rule (mirrors tui/linux/web/android/apple).
    /// </summary>
    private HashSet<string> _visibleCalendarIds = new();
    public IReadOnlyCollection<string> VisibleCalendarIds => _visibleCalendarIds;

    /// <summary>
    /// The last fetched, UNFILTERED union — <see cref="Events"/> is re-derived
    /// from this on every fetch AND every `calendar-visibility` toggle
    /// (<see cref="PublishDisplayedEvents"/>), so toggling a box is purely
    /// local (no refetch: this cache already holds every owned calendar's
    /// events plus the invited feed on the no-selection union arm).
    /// </summary>
    private List<EventInfo> _rawEvents = new();

    /// <summary><c>calendar-date-label</c> — the VISIBLE RANGE, not always the
    /// month.
    ///
    /// <para>It rendered the month in every mode until 2026-08-24, which made it
    /// useless as an observable for the finer modes: seven day-view pans inside
    /// one month left it unchanged. That is not only an assertion problem — the
    /// e2e helper <c>actions/events.py::prev_month</c>/<c>next_month</c> WAITS
    /// for this text to change, so a month-only label times the helper out
    /// rather than merely mis-reporting. linux's <c>update_date_label</c> and
    /// apple's <c>EventsVM.refreshDateLabel()</c> are the worked examples.</para>
    ///
    /// <para>Agenda is date-unfiltered, so it names the LIST rather than a date —
    /// there is no range to describe. The month/day names come from .NET's
    /// locale-aware formatting (the platform date library, as
    /// <c>events.md</c> § Where logic lives allows the five non-Rust apps);
    /// only the agenda word is a catalog string, since it is ours and not a
    /// date at all.</para></summary>
    public string MonthYearDisplay => _viewMode switch
    {
        FfiCalendarViewMode.Month => CurrentMonth.ToString("MMMM yyyy"),
        FfiCalendarViewMode.Week => WeekRangeLabel(CurrentDay),
        FfiCalendarViewMode.Day => CurrentDay.ToString("d MMMM yyyy"),
        _ => Strings.Get("common/upcoming"),
    };

    /// <summary>The week view's label: the visible week's span. Snaps through the
    /// shared <see cref="FaunaApp.Core.Calendar.WeekStart"/> — the same probe and
    /// rotation both calendar grids use — so the span can never name a week the
    /// grid does not draw.</summary>
    private static string WeekRangeLabel(DateTimeOffset anchor)
    {
        var start = FaunaApp.Core.Calendar.WeekStart.StartOfWeek(anchor);
        var end = start.AddDays(6);
        return start.Month == end.Month
            ? $"{start:d} – {end:d MMMM yyyy}"
            : $"{start:d MMM} – {end:d MMM yyyy}";
    }

    // Calendars and events ride the encrypted CalDAV store the mail-bridge MDA
    // serves, via the shared FfiCaldavClient seam (the same store apple/android/
    // web/linux read — events.md § Encrypted store). Ids are lowercase hex.
    private readonly INestRpcClient _rpc;

    internal EventsViewModel(INestRpcClient rpc)
    {
        _rpc = rpc;
    }

    [RelayCommand]
    private async Task LoadAsync()
    {
        IsLoading = true;
        ErrorMessage = null;
        try
        {
            var calendars = await _rpc.CaldavListCalendarsAsync();
            // The CalDAV calendar id is already lowercase hex; color is "" when
            // the sealed metadata carried none. The encrypted store has no
            // per-calendar timezone, so CalendarInfo.Timezone is null.
            var previous = Calendars.ToList();
            var next = calendars.Select(c => new CalendarInfo(c.id, c.name, c.color, null)).ToList();
            _visibleCalendarIds = SeedVisibleCalendarIds(_visibleCalendarIds, previous, next);
            Calendars.Clear();
            foreach (var c in next) Calendars.Add(c);
            RebuildCalendarRows();
            await QueryEventsAsync();
        }
        catch (Exception ex)
        {
            ShowError(ex);
        }
        finally
        {
            IsLoading = false;
        }
    }

    /// <summary><c>events-prev-month</c> / <c>events-next-month</c> — move
    /// <b>one visible range</b> per click, in whatever mode is showing
    /// (<c>events.md</c> § Where logic lives → *View mode + visible range*).
    /// <paramref name="direction"/> is the SIGN only (-1 / +1).
    ///
    /// <para>The distance is the shared policy — <c>calendar_pan_step</c> answers
    /// the magnitude alone (<c>Months{1}</c> / <c>Days{7}</c> / <c>Days{1}</c> /
    /// <c>None</c>) — and the walk is .NET's <c>AddMonths</c>/<c>AddDays</c>. That
    /// split is deliberate and load-bearing: the goal doc keeps Gregorian date
    /// math on each platform's own date library, so this must NOT call the Rust
    /// <c>pan()</c> (which is not exported here for exactly that reason).</para>
    ///
    /// <para>⚠ <c>None</c> is <b>"do nothing"</b>, not "move by zero". The agenda
    /// list is date-unfiltered, so it ignores the anchor entirely — a click that
    /// quietly moved the anchor would change state nothing renders, and then
    /// show the user a range they never navigated to on their next mode switch.
    /// The arm returns before touching the anchor, the label, or the query.</para>
    ///
    /// <para>This one path replaced three: <c>NavigateMonth</c> stepped a month
    /// in EVERY mode, while <c>NavigateWeek</c>/<c>NavigateDay</c> existed with
    /// no production caller at all — per-mode copies of the very policy now read
    /// from shared Rust. Both anchors move together here, which also closes the
    /// stale-<c>CurrentDay</c> bug the month-only path left behind (pan in month
    /// view, switch to week, and the week grid showed the pre-pan day).</para></summary>
    [RelayCommand]
    private async Task NavigateAsync(int direction)
    {
        var step = FaunaFfiMethods.CalendarPanStep(_viewMode);
        DateTimeOffset moved;
        switch (step)
        {
            case FfiPanStep.Months m:
                moved = CurrentDay.AddMonths(direction * m.@count);
                break;
            case FfiPanStep.Days d:
                moved = CurrentDay.AddDays(direction * d.@count);
                break;
            default:
                return;   // FfiPanStep.None — do nothing at all.
        }

        // Lockstep, not two independent anchors: the month grid reads
        // CurrentMonth and the week/day grids read CurrentDay, so moving one
        // without the other is what let a mode switch land on a stale range.
        CurrentDay = moved;
        CurrentMonth = moved;
        OnPropertyChanged(nameof(MonthYearDisplay));
        IsLoading = true;
        ErrorMessage = null;
        try { await QueryEventsAsync(); }
        catch (Exception ex) { ShowError(ex); }
        finally { IsLoading = false; }
    }

    [RelayCommand]
    private async Task CreateEventAsync(EventDraft draft)
    {
        ErrorMessage = null;
        // A calendar is required (create_event needs the calendar id). The create
        // form is only reachable with calendars present; fall back to the first
        // calendar if none is explicitly selected, and no-op if there are none.
        var cal = SelectedCalendar ?? Calendars.FirstOrDefault();
        if (cal is null) return;
        try
        {
            await _rpc.CaldavCreateEventAsync(
                cal.Id, draft.Summary, draft.Dtstart, draft.Dtend ?? "",
                draft.Location, draft.Description);
            await LoadAsync();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    [RelayCommand]
    private async Task CreateCalendarAsync(string name)
    {
        ErrorMessage = null;
        try
        {
            await _rpc.CaldavCreateCalendarAsync(name);
            await LoadAsync();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    // Event deletion lives on EventDetailViewModel.DeleteCommand (the canonical
    // card → event_detail flow); the agenda card no longer has an inline delete.

    [RelayCommand]
    private async Task RsvpAsync(string eventIdAndStatus)
    {
        var parts = eventIdAndStatus.Split(':', 2);
        if (parts.Length != 2) return;
        ErrorMessage = null;
        try
        {
            var response = Enum.Parse<uniffi.fauna_core.RsvpResponse>(parts[1], ignoreCase: true);
            await _rpc.CaldavRsvpEventAsync(parts[0], response);
            await LoadAsync();
        }
        catch (Exception ex) { ShowError(ex); }
    }

    /// <summary>
    /// Single-click a month-grid day cell → switch to Day view for that date
    /// (Microsoft Outlook's month→day drill-in; docs/goal/ui/events.md
    /// § User actions, the <c>events-day-cell-{date}</c> row). Synchronous and
    /// re-query-free: the events list is unbounded-loaded over WS-RPC and the
    /// day view filters client-side, so this is pure view-mode + date state —
    /// the view-mode + visible-range glue mirrors the per-app
    /// <c>calendar-view-*</c> toggles (lifts into shared Rust with the Step 4c
    /// events lift). No <c>ConfigureAwait(false)</c> anywhere on this VM path:
    /// off-thread bound-state mutation throws COMException on WinUI.
    /// </summary>
    public void SelectDay(DateTimeOffset date)
    {
        // ORDER IS LOAD-BEARING: the date must be in place BEFORE ViewMode.
        // CommunityToolkit.Mvvm raises PropertyChanged synchronously, and the
        // page rebuilds the day timeline inside its ViewMode observer, reading
        // CurrentDay as it goes (EventsPage.xaml.cs::BuildDayTimeline). Setting
        // ViewMode first therefore built the timeline for the PREVIOUS day —
        // the drill-in landed on today rather than the clicked cell, and no
        // observer fires on CurrentDay to correct it, so it stayed wrong until
        // the ~10s poll happened to rebuild. That is the wrong-dated event the
        // month-grid double-click reported: the second physical click landed on
        // a time slot of a timeline built for the wrong day. Pinned by
        // EventsViewModelDayCellTests::SelectDay_SetsTheDate_BeforeViewModeIsObserved.
        CurrentDay = date;
        CurrentMonth = date;
        ViewMode = FfiCalendarViewMode.Day;
        OnPropertyChanged(nameof(MonthYearDisplay));
    }

    /// <summary>
    /// Double-click a month-grid day cell → prepare the new-event compose for
    /// that date (Microsoft Outlook's double-click-empty-day → new event;
    /// docs/goal/ui/events.md § User actions, the <c>events-day-cell-{date}</c>
    /// double-click row). Sets <see cref="ComposeStartPrefill"/> to the cell
    /// date at the shared working-day start (<c>FaunaFfiMethods.WorkingDayStart()</c>,
    /// <c>fauna_core::caltime::WORKING_DAY_START</c>, 09:00 — as every app uses,
    /// mirroring apple's <c>beginCompose(onDay:)</c>) in the combined
    /// <c>YYYY-MM-DDTHH:MM</c>
    /// shape <c>event-dtstart</c> accepts; the page observes this property to
    /// open the compose panel and fill the start field (client glue, mirroring
    /// <see cref="SelectDay"/> — the view-mode/date + compose glue lifts into
    /// shared Rust with the Step 4c events lift). Synchronous; no
    /// <c>ConfigureAwait(false)</c> on this VM path (off-thread bound-state
    /// mutation throws COMException on WinUI).
    /// </summary>
    public void NewEventOnDay(DateTimeOffset date)
    {
        var workingStart = FaunaFfiMethods.WorkingDayStart();
        NewEventAtSlot(date, (int)workingStart.@hour, (int)workingStart.@minute);
    }

    /// <summary>
    /// Click an empty week/day-grid time slot → prepare the new-event compose
    /// for that date at the slot's own snapped time (docs/goal/ui/events.md
    /// § Week &amp; day timeline views: "Clicking an empty time slot opens the
    /// new-event compose prefilled at that date/time" — the same Outlook
    /// empty-space drill-in the month grid gets from
    /// <see cref="NewEventOnDay"/>, which is this call at a 09:00 default).
    /// The hour/minute come CLOSURE-CAPTURED from the clicked marker, never
    /// derived from pointer y: a headless driver's gesture carries no
    /// meaningful coordinates, so a y-computed time would always read 00:00
    /// (linux's reference leg makes the same choice for the same reason).
    /// Synchronous; no <c>ConfigureAwait(false)</c> on this VM path (off-thread
    /// bound-state mutation throws COMException on WinUI).
    /// </summary>
    public void NewEventAtSlot(DateTimeOffset date, int hour, int minute)
    {
        ComposeStartPrefill = $"{date:yyyy-MM-dd}T{hour:D2}:{minute:D2}";
    }

    /// <summary>
    /// Monotonic generation token guarding <see cref="Events"/> against the
    /// stale-query-overwrites-narrowed-selection race (events.md § Implementation
    /// status today, the no-selection-union row; apple's <c>4e5dac926</c>,
    /// mirrored by android's <c>agendaGen</c> / web's <c>queryGen</c> / tui's
    /// <c>events_gen</c>). Creating a calendar reloads with no selection (the
    /// SLOW N-calendar union, via <see cref="LoadAsync"/>'s own inner call);
    /// tapping the new calendar fires a second, independent, FAST single-calendar
    /// <see cref="LoadAsync"/> concurrently (<c>EventsPage.CalendarList_SelectionChanged</c>).
    /// With no ordering guard the fast reply could paint correctly and then the
    /// slow union lands on top of it, silently re-widening the view. Claimed
    /// synchronously right before the await in <see cref="QueryEventsAsync"/>;
    /// <see cref="RefreshIfChangedAsync"/> (the background poll) only OBSERVES
    /// it, never claims — claiming there could invalidate an in-flight
    /// foreground publish with nothing published in its place (the exact
    /// regression apple's fix write-up flags).
    /// </summary>
    private int _eventsGen;

    /// <summary>
    /// Query the events over the encrypted CalDAV store. When a calendar is
    /// selected, only its events; otherwise the union of every owned calendar's
    /// events plus the invited-events feed (deduped by hex id) — the encrypted
    /// store has no cross-calendar query, so the VM fans out. Unbounded in time
    /// (the agenda lists all; the month/week/day grids filter client-side by
    /// <c>dtstart</c> — see <c>EventsPage.BuildMonthGrid</c>), so the result is
    /// independent of which view is active.
    /// </summary>
    private async Task QueryEventsAsync()
    {
        var gen = ++_eventsGen; // claim BEFORE the await — see _eventsGen
        var items = await QueryEventItemsAsync();
        if (gen != _eventsGen) return; // a newer claim landed since; drop this stale reply
        _rawEvents = BuildEventInfos(items);
        PublishDisplayedEvents();
    }

    /// <summary>
    /// Fetch the raw event rows over the encrypted CalDAV store (no UI mutation).
    /// Split out of <see cref="QueryEventsAsync"/> so the background refresh
    /// (<see cref="RefreshIfChangedAsync"/>) can fetch + change-detect before
    /// touching the bound <see cref="Events"/> collection. See QueryEventsAsync
    /// for the fan-out rationale (the encrypted store has no cross-calendar query).
    /// </summary>
    private async Task<IReadOnlyList<uniffi.fauna_ffi.FfiCalEvent>> QueryEventItemsAsync()
    {
        // Resolve the selection against the calendars that actually exist right
        // now, at READ time — never by mutating SelectedCalendar itself (events.md
        // § Where logic lives → "Which calendars the page is scoped to"). A
        // selection naming a calendar deleted here or by an external CalDAV MUA
        // against the same bridge_caldav_* store falls back to the union instead
        // of silently querying a gone calendar and reading empty with no error;
        // mirrors android's EventsVM.kt:376.
        var existingIds = Calendars.Select(c => c.Id).ToArray();
        var resolvedId = uniffi.fauna_ffi.FaunaFfiMethods.ResolveCalendarSelection(
            SelectedCalendar?.Id, existingIds);
        if (resolvedId is { } id)
        {
            return await _rpc.CaldavQueryEventsAsync(id);
        }
        var byId = new Dictionary<string, uniffi.fauna_ffi.FfiCalEvent>();
        foreach (var c in Calendars)
            foreach (var ev in await _rpc.CaldavQueryEventsAsync(c.Id))
                byId[ev.id] = ev;
        foreach (var ev in await _rpc.CaldavQueryInvitedEventsAsync())
            byId[ev.id] = ev;
        return byId.Values.ToList();
    }

    /// <summary>
    /// Build the <see cref="EventInfo"/> rows from raw event items — pure, no
    /// mutation of <see cref="Events"/>. Calendar colors come from the current
    /// <see cref="Calendars"/>, so callers must update calendars BEFORE building
    /// events (matches the LoadAsync order).
    /// </summary>
    private List<EventInfo> BuildEventInfos(IReadOnlyList<uniffi.fauna_ffi.FfiCalEvent> items)
    {
        var rows = new List<EventInfo>(items.Count);
        foreach (var ev in items)
        {
            var start = ParseDate(ev.dtstart);
            var end = ev.dtend is { } de ? ParseDate(de) : start.AddHours(1);
            var calColor = Calendars.FirstOrDefault(c => c.Id == ev.calendarId)?.Color;
            // The per-actor RSVP isn't on the event summary (consistent with the
            // detail view's note); the agenda card carries no RSVP badge status.
            rows.Add(new EventInfo(
                ev.id, ev.summary, start, end, ev.description, ev.location,
                ev.calendarId, RsvpStatus: "", calColor,
                IsAllDay: uniffi.fauna_ffi.FaunaFfiMethods.EventIsAllDay(ev.dtstart, ev.dtend)));
        }
        return rows;
    }

    /// <summary>
    /// Re-derive <see cref="Events"/> from <see cref="_rawEvents"/> through the
    /// `calendar-visibility` filter — called after every fetch AND every toggle,
    /// so a toggle never needs to refetch.
    /// </summary>
    private void PublishDisplayedEvents()
    {
        Events.Clear();
        foreach (var info in _rawEvents)
            if (IsCalendarDisplayed(info))
                Events.Add(info);
    }

    /// <summary>
    /// Does <paramref name="info"/> belong on the page right now? UniFFI face of
    /// <c>fauna_client_caldav::calendar_is_displayed</c> (events.md § Where logic
    /// lives → *Which calendars display*): a live <see cref="SelectedCalendar"/>
    /// selection wins outright; with none, <see cref="_visibleCalendarIds"/>
    /// filters the union, an EMPTY set meaning "no filter" (never "hide
    /// everything"). An event with no <c>CalendarId</c> is never hidden
    /// (defensive only — the seam always populates it).
    ///
    /// windows is the one app that unions the invited-events feed into this same
    /// list at fetch time (<see cref="QueryEventItemsAsync"/>) rather than
    /// keeping it in a separate always-visible section the way apple/android/tui
    /// do — so an event whose calendar is not one of <see cref="Calendars"/> is
    /// necessarily a foreign/invited one, and the display filter (a MY-calendars
    /// concept) never reaches it.
    /// </summary>
    private bool IsCalendarDisplayed(EventInfo info)
    {
        if (info.CalendarId is not { } calId) return true;
        var existingIds = Calendars.Select(c => c.Id).ToArray();
        if (!existingIds.Contains(calId)) return true;
        return uniffi.fauna_ffi.FaunaFfiMethods.CalendarIsDisplayed(
            SelectedCalendar?.Id, existingIds, _visibleCalendarIds.ToArray(), calId);
    }

    /// <summary>
    /// Flip one calendar's `calendar-visibility` checkbox — purely local display
    /// state; re-filters the already-fetched <see cref="_rawEvents"/>, no refetch.
    /// </summary>
    public void ToggleCalendarVisibility(string calendarId)
    {
        if (!_visibleCalendarIds.Remove(calendarId))
            _visibleCalendarIds.Add(calendarId);
        foreach (var row in CalendarRows)
            row.IsVisible = _visibleCalendarIds.Count == 0 || _visibleCalendarIds.Contains(row.Calendar.Id);
        PublishDisplayedEvents();
    }

    private void RebuildCalendarRows()
    {
        CalendarRows.Clear();
        foreach (var cal in Calendars)
            CalendarRows.Add(new CalendarRowVm
            {
                Calendar = cal,
                IsVisible = _visibleCalendarIds.Count == 0 || _visibleCalendarIds.Contains(cal.Id),
            });
    }

    /// <summary>
    /// Fill <paramref name="visible"/> on a calendar list that just replaced
    /// <paramref name="previous"/> with <paramref name="next"/>, so the page
    /// paints every box checked instead of leaning on the empty-set-is-union
    /// rule (mirrors tui/linux/web/android/apple's own seed function). A
    /// BRAND-NEW calendar (not in <paramref name="previous"/>) starts visible;
    /// an existing one keeps whatever the user chose. Unchecking every box
    /// empties the set, which the shared predicate reads as the union — so the
    /// next load re-seeds it (an accepted quirk shared by every app that
    /// implements this exact algorithm, not a windows-specific bug).
    /// </summary>
    internal static HashSet<string> SeedVisibleCalendarIds(
        IReadOnlyCollection<string> visible,
        IReadOnlyList<CalendarInfo> previous,
        IReadOnlyList<CalendarInfo> next)
    {
        var previousIds = previous.Select(c => c.Id).ToHashSet();
        var seeded = new HashSet<string>(visible);
        foreach (var cal in next)
            if (!previousIds.Contains(cal.Id))
                seeded.Add(cal.Id);
        if (seeded.Count == 0)
            foreach (var cal in next)
                seeded.Add(cal.Id);
        return seeded;
    }

    // ── Change-detection background refresh (Events-page poll) ─────────────────

    /// <summary>
    /// Order-insensitive equality of two row sequences keyed by a stable id, used
    /// to decide whether a background refresh actually changed anything (mirrors
    /// linux <c>rows_differ_by_id</c>, apps/fauna-linux/src/app.rs). The Events-page
    /// poll re-fetches on a fixed cadence; without this gate a steady-state poll
    /// would clear+refill the bound collection every interval, destroying and
    /// rebuilding every <c>calendar-item</c>/<c>event-card</c> (flicker + a race
    /// with an in-progress list click). Sorting by id first makes a stable nest
    /// reply order irrelevant; <c>CalendarInfo</c>/<c>EventInfo</c> are records, so
    /// the element compare also catches a content edit (a renamed/retimed row keeps
    /// its id but the value differs). Returns true when the sequences differ.
    /// </summary>
    internal static bool RowsDifferById<T>(IReadOnlyList<T> a, IReadOnlyList<T> b, Func<T, string> id)
    {
        if (a.Count != b.Count) return true;
        var sa = a.OrderBy(id, StringComparer.Ordinal).ToList();
        var sb = b.OrderBy(id, StringComparer.Ordinal).ToList();
        var cmp = EqualityComparer<T>.Default;
        for (int i = 0; i < sa.Count; i++)
            if (!cmp.Equals(sa[i], sb[i])) return true;
        return false;
    }

    /// <summary>Which slices a <see cref="RefreshIfChangedAsync"/> actually changed.</summary>
    public readonly record struct EventsRefreshResult(bool CalendarsChanged, bool EventsChanged)
    {
        public bool AnyChanged => CalendarsChanged || EventsChanged;
    }

    /// <summary>
    /// Silent background refresh for the Events-page poll: re-fetch calendars +
    /// events and clear+refill the bound collections ONLY when the id-set or a
    /// shown field actually changed, so an externally-created CalDAV calendar/event
    /// surfaces while the user stays on the page (docs/goal/ui/events.md
    /// § Implementation status — the "Quick-appearance refresh" follow-on (c);
    /// mirrors the linux poll + CalendarsLoaded/EventsLoaded change-detection arms).
    /// In steady state (nothing changed) this is a UI no-op — no collection
    /// mutation, no <see cref="EventInfo"/> rebuild — which keeps a stable poll from
    /// flickering the page or racing a click.
    ///
    /// Deliberately does NOT touch <see cref="IsLoading"/> (a background poll must
    /// not flash the spinner), and swallows transient RPC errors (keep the last-good
    /// view; the next tick retries) — returning what changed so far. NO
    /// <c>ConfigureAwait(false)</c> anywhere on this VM path: off-thread bound-state
    /// mutation throws a silent COMException on WinUI.
    /// </summary>
    public async Task<EventsRefreshResult> RefreshIfChangedAsync()
    {
        bool calendarsChanged = false;
        bool eventsChanged = false;
        try
        {
            // Calendars first, so the event build below picks up current colors
            // (matches the LoadAsync order). The fetched-vs-shown compare keeps the
            // sidebar widgets intact when the calendar set is unchanged.
            var fetched = await _rpc.CaldavListCalendarsAsync();
            var newCalendars = new List<CalendarInfo>(fetched.Count);
            foreach (var c in fetched)
                newCalendars.Add(new CalendarInfo(c.id, c.name, c.color, null));
            if (RowsDifferById(Calendars, newCalendars, c => c.Id))
            {
                var previous = Calendars.ToList();
                _visibleCalendarIds = SeedVisibleCalendarIds(_visibleCalendarIds, previous, newCalendars);
                Calendars.Clear();
                foreach (var c in newCalendars) Calendars.Add(c);
                RebuildCalendarRows();
                calendarsChanged = true;
            }

            // Then events (using the now-current calendars + SelectedCalendar scope).
            // OBSERVE _eventsGen, never claim it (see _eventsGen's doc comment): if a
            // foreground QueryEventsAsync claimed a newer generation while this fetch
            // was in flight, that publish is authoritative for this epoch — drop this
            // poll's reply rather than clobbering it or racing to publish first.
            // Compared RAW-vs-RAW (never against the filtered Events) so a
            // calendar-visibility toggle can never look like an external change to
            // the poll, and the poll's own diff can never fight a local toggle.
            var observedGen = _eventsGen;
            // A live single-calendar selection consults the CalDAV delta-sync
            // backstop (events.md § Implementation status today) instead of paying
            // for a full unseal+decode every tick — `null` means the seam's own
            // sync-token probe says this ONE calendar hasn't changed since the last
            // poll, so the caller's already-rendered list stays authoritative.
            // NOT applied to the union case below: windows folds the
            // invited-events feed into the SAME list at fetch time (unlike
            // apple/android/tui, which keep it in a separate always-visible
            // section — see IsCalendarDisplayed's own doc comment), and the seam
            // has no coverage for invited events yet, so skipping the read there
            // could miss a new invite with no owned-calendar change to signal it.
            var existingIds = Calendars.Select(c => c.Id).ToArray();
            var resolvedId = uniffi.fauna_ffi.FaunaFfiMethods.ResolveCalendarSelection(
                SelectedCalendar?.Id, existingIds);
            List<EventInfo>? newRawEvents;
            if (resolvedId is { } id)
            {
                var seeded = await _rpc.CaldavQueryEventsSeededAsync(id);
                newRawEvents = seeded is null ? null : BuildEventInfos(seeded);
            }
            else
            {
                newRawEvents = BuildEventInfos(await QueryEventItemsAsync());
            }
            if (newRawEvents is not null
                && observedGen == _eventsGen
                && RowsDifferById(_rawEvents, newRawEvents, e => e.Id))
            {
                _rawEvents = newRawEvents;
                PublishDisplayedEvents();
                eventsChanged = true;
            }
        }
        catch
        {
            // Transient RPC failure (a flaky tick): keep the last-good view and
            // report what changed so far; the next poll tick retries.
        }
        return new EventsRefreshResult(calendarsChanged, eventsChanged);
    }

    private static DateTimeOffset ParseDate(string value)
    {
        if (DateTimeOffset.TryParse(value, out var dto)) return dto;
        return DateTimeOffset.MinValue;
    }
}
