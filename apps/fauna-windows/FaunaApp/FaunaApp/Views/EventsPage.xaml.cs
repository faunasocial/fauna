using System.Globalization;
using System.Linq;
using System.Text.Json;
using Microsoft.UI.Xaml;
using Microsoft.UI.Xaml.Automation;
using Microsoft.UI.Xaml.Controls;
using Microsoft.UI.Xaml.Data;
using Microsoft.UI.Xaml.Navigation;
using FaunaApp.Core.Calendar;
using uniffi.fauna_ffi;
using FaunaApp.Core.Logs;
using FaunaApp.Core.Models;
using FaunaApp.Core.Services;
using FaunaApp.Core.ViewModels;
using FaunaApp.Services;
using S = FaunaApp.Core.Services.Strings;
using FaunaApp.UiIds;

namespace FaunaApp.Views;

public sealed partial class EventsPage : Page
{
    private EventsViewModel? _viewModel;
    private ServiceClients? _clients;

    // Time-grid geometry: 30 px per half-hour → 1440 px column height.
    private const double HalfHourPx = 30.0;
    private const double QuarterHourPx = HalfHourPx / 2;   // one empty-slot marker
    private const double ColumnHeightPx = 48 * HalfHourPx; // 1440
    private const double HourGutterWidth = 56.0;

    // ~10s change-detection poll so an event/calendar created by an EXTERNAL
    // CalDAV client surfaces while the user stays on the page (events.md
    // § Implementation status — the "Quick-appearance refresh" follow-on (c);
    // mirrors linux CALENDAR_POLL_INTERVAL_SECS). The VM's RefreshIfChangedAsync
    // is a UI no-op in steady state, so the tick is cheap on every idle interval.
    private const int PollIntervalSecs = 10;
    private DispatcherTimer? _pollTimer;

    // Month day-cell single-vs-double click arbitration (events.md § Layout &
    // flow). The drill-in is deferred by the user's own double-click time so the
    // cell is still mounted when the second half of a double lands on it; the
    // decision itself lives in FaunaApp.Core so it is pinned by tier_1 tests
    // with a fake clock (DayCellClickArbiterTests) rather than by a test racing
    // a wall clock. See DayCellClickArbiter for why acting on the first click
    // made the double-click gesture unreachable for a real user.
    [System.Runtime.InteropServices.DllImport("user32.dll")]
    private static extern uint GetDoubleClickTime();

    private readonly DayCellClickArbiter _dayCellClicks =
        new(TimeSpan.FromMilliseconds(GetDoubleClickTime()));
    private DispatcherTimer? _drillInTimer;

    public EventsPage()
    {
        this.InitializeComponent();
        // Cache this page so the agenda's selected-calendar scope survives a
        // round-trip to EventDetailPage. Without it, GoBack re-creates the VM
        // with SelectedCalendar=null → the reload hits the nest's UNSCOPED
        // events.query "owned" branch, which (unlike the calendar branch) does
        // NOT exclude just-deleted events, so a deleted card resurfaces (the
        // deeper nest fix is tracked internally).
        this.NavigationCacheMode = NavigationCacheMode.Required;
        EventsTitle.Text = S.Get("events/title");
        AgendaButton.Content = S.Get("events/view_agenda");
        MonthButton.Content = S.Get("events/view_month");
        WeekButton.Content = S.Get("events/view_week");
        DayButton.Content = S.Get("events/view_day");
        CalendarNameBox.PlaceholderText = S.Get("events/calendar_name");
        CreateCalendarButton.Content = S.Get("common/create");
        EventSummaryBox.PlaceholderText = S.Get("events/summary");
        // Combined date+time fields are free-text (no native single combined
        // WinUI datetime picker); show the accepted ISO format as a hint.
        EventStartBox.PlaceholderText = "YYYY-MM-DDTHH:MM";
        EventEndBox.PlaceholderText = "YYYY-MM-DDTHH:MM";
        EventLocationBox.PlaceholderText = S.Get("events/location_placeholder");
        CreateEventButton.Content = S.Get("common/create");

        // Draft-persistence v2, events leg (reserved-folders.md § Drafts Sync;
        // events.md § Persistence). Wired once here (the constructor runs once per
        // page instance, and this page is NavigationCacheMode.Required — never
        // rebuilt within a session), not per OnNavigatedTo/Page_Loaded re-entry.
        EventSummaryBox.TextChanged += EventComposeField_TextChanged;
        EventStartBox.TextChanged += EventComposeField_TextChanged;
        EventEndBox.TextChanged += EventComposeField_TextChanged;
        EventDescriptionBox.TextChanged += EventComposeField_TextChanged;
        EventLocationBox.TextChanged += EventComposeField_TextChanged;
    }

    /// <summary>
    /// Adapts this page's five compose <c>TextBox</c>es to <see cref="IEventDraftStore"/>.
    /// Unlike the conversations/feed legs' manager-backed stores (thread-safe Rust calls
    /// across the UniFFI boundary), this store touches WinUI controls directly, and
    /// <see cref="EventDraftsService"/>'s debounce fires on a <see cref="System.Threading.Timer"/>
    /// threadpool callback — never the UI thread — so every access here marshals onto
    /// this page's <c>DispatcherQueue</c> first. There is no shared object to snapshot
    /// otherwise: this page's boxes ARE the live draft (EventDraftsService's own doc
    /// comment), which is sound specifically because this page is cached and its boxes
    /// are never torn down within a session.
    /// </summary>
    private sealed class PageDraftStore : IEventDraftStore
    {
        private readonly EventsPage _page;
        public PageDraftStore(EventsPage page) => _page = page;

        public FfiEventDrafts Snapshot() => RunOnUiThread(() => new FfiEventDrafts(
            _page.EventSummaryBox.Text ?? "",
            _page.EventStartBox.Text ?? "",
            _page.EventEndBox.Text ?? "",
            _page.EventDescriptionBox.Text ?? "",
            _page.EventLocationBox.Text ?? ""));

        public void Restore(FfiEventDrafts draft) => RunOnUiThread(() =>
        {
            _page.EventSummaryBox.Text = draft.summary;
            _page.EventStartBox.Text = draft.dtstart;
            _page.EventEndBox.Text = draft.dtend;
            _page.EventDescriptionBox.Text = draft.description;
            _page.EventLocationBox.Text = draft.location;
        });

        public bool HasAuthoredText() => RunOnUiThread(() =>
            !string.IsNullOrEmpty(_page.EventSummaryBox.Text)
            || !string.IsNullOrEmpty(_page.EventStartBox.Text)
            || !string.IsNullOrEmpty(_page.EventEndBox.Text)
            || !string.IsNullOrEmpty(_page.EventDescriptionBox.Text)
            || !string.IsNullOrEmpty(_page.EventLocationBox.Text));

        /// <summary>Run <paramref name="func"/> on this page's UI thread, blocking the
        /// caller until it completes — a synchronous WinUI cross-thread bridge for a
        /// caller (the debounce timer) that cannot itself be made async. A no-op when
        /// already on the UI thread (the restore path, which runs inline off an awaited
        /// continuation that happens not to have hopped threads).</summary>
        private T RunOnUiThread<T>(Func<T> func)
        {
            if (_page.DispatcherQueue.HasThreadAccess) return func();
            var tcs = new TaskCompletionSource<T>();
            if (!_page.DispatcherQueue.TryEnqueue(() =>
            {
                try { tcs.SetResult(func()); }
                catch (Exception ex) { tcs.SetException(ex); }
            }))
            {
                // The queue is shutting down (page/window torn down) — nothing left to
                // marshal onto; the caller gets a harmless empty/default answer rather
                // than hanging forever waiting for a queue that will never run it.
                return default!;
            }
            return tcs.Task.GetAwaiter().GetResult();
        }

        private void RunOnUiThread(Action action) => RunOnUiThread<object?>(() => { action(); return null; });
    }

    private void EventComposeField_TextChanged(object sender, TextChangedEventArgs e)
    {
        Core.Logs.E2eTrace.Write($"[event-drafts] TextChanged: App.EventDrafts={(App.EventDrafts is null ? "null" : "set")}");
        App.EventDrafts?.ScheduleSave();
    }

    /// <summary>
    /// Build the events-rail draft service paired with THIS page's compose surface and
    /// restore any stored draft — mirrors <c>FeedPage.Page_Loaded</c>'s pairing, but
    /// runs only ONCE per page instance (guarded by the same <c>_viewModel is null</c>
    /// first-navigation check its caller uses), not on every re-entry: unlike
    /// <c>FfiFeedManager</c>, this page's compose surface is never rebuilt within a
    /// session, so a second service instance would only orphan the first one's pending
    /// debounce timer. Best-effort — see <see cref="EventDraftsService.RestoreOnLaunchAsync"/>.
    /// </summary>
    private async Task WireEventDraftsAsync()
    {
        Core.Logs.E2eTrace.Write($"[event-drafts] EventsPage.WireEventDraftsAsync starting; EventDraftsSyncTask={(App.EventDraftsSyncTask is null ? "null" : "set")}, EventDraftsSync={(App.EventDraftsSync is null ? "null" : "set")}");
        // Bounded-await the e2e login's fire-and-forget kickoff (App.EventDraftsSyncTask's
        // own doc comment): under production this is always null (StartMainAppAsync
        // already awaited WireEventDraftsAsync synchronously before login finished), but
        // under the e2e set_state path — which never reaches StartMainAppAsync — a test
        // navigating to Events soon after login can otherwise race ahead of
        // App.EventDraftsSync still being null. Same 10s ceiling as the feed leg's
        // AwaitE2eFeedDraftsSyncAsync.
        if (App.EventDraftsSyncTask is { } pending)
            await Task.WhenAny(pending, Task.Delay(TimeSpan.FromSeconds(10)));
        Core.Logs.E2eTrace.Write($"[event-drafts] after bounded-await; EventDraftsSync={(App.EventDraftsSync is null ? "null" : "set")}");
        if (App.EventDraftsSync is not { } sync) return;
        var service = new EventDraftsService(sync, new PageDraftStore(this));
        App.EventDrafts = service;
        await service.RestoreOnLaunchAsync();
        Core.Logs.E2eTrace.Write("[event-drafts] EventsPage.WireEventDraftsAsync completed, App.EventDrafts set");
    }

    protected override void OnNavigatedTo(NavigationEventArgs e)
    {
        base.OnNavigatedTo(e);
        // Build the VM once. On a Back navigation from EventDetailPage the page
        // is cached (NavigationCacheMode.Required), so _viewModel is non-null and
        // we keep it — preserving SelectedCalendar (and thus the calendar-scoped
        // query). Page_Loaded re-runs on every re-entry and refreshes the list.
        if (e.Parameter is ServiceClients clients && _viewModel is null)
        {
            _clients = clients;
            _viewModel = new EventsViewModel(clients.Rpc!);
            _viewModel.PropertyChanged += ViewModel_PropertyChanged;
            _viewModel.CalendarRows.CollectionChanged += (s, args) =>
            {
                CalendarList.Visibility = _viewModel.CalendarRows.Count > 0
                    ? Visibility.Visible : Visibility.Collapsed;
            };
            // fauna.calendar.changed push (transport.md § Push events):
            // re-run the SAME ~10s poll refresh+rebuild path immediately rather
            // than waiting for the next tick. Subscribed once here (this block
            // only runs on first navigation — the page is cached,
            // NavigationCacheMode.Required) and never unsubscribed: the single
            // cached EventsPage instance IS the app-lifetime instance, so
            // there's no second instance to leak onto.
            //
            // ALSO subscribe to Reconnected (closed 2026-09-06, transport.md §
            // Which surfaces a push invalidates — the windows-leg audit):
            // Events used to sit entirely outside the reconnect sweep, so a
            // dropped fauna.calendar.changed push across a socket gap stayed
            // unrecovered until the next poll tick — neither an ordinary
            // reconnect nor a ResyncRequired sweep ever refreshed this page.
            if (clients.Rpc is not null)
            {
                clients.Rpc.CalendarPushChanged += OnCalendarPushChanged;
                clients.Rpc.Reconnected += OnReconnected;
            }

            // Draft-persistence v2, events leg: fire-and-forget (OnNavigatedTo is not
            // async), guarded by the same first-navigation branch as the rest of this
            // block's one-time setup — see WireEventDraftsAsync's own doc comment for
            // why this never re-runs on a later re-entry.
            _ = WireEventDraftsAsync();
        }
    }

    private void OnCalendarPushChanged(string actorId, string calendarId) =>
        Poll_Tick(this, EventArgs.Empty);

    private void OnReconnected() => Poll_Tick(this, EventArgs.Empty);

    private async void Page_Loaded(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        MonthYearText.Text = _viewModel.MonthYearDisplay;
        await _viewModel.LoadCommand.ExecuteAsync(null);
        AgendaPanel.ItemsSource = _viewModel.Events;
        CalendarList.ItemsSource = _viewModel.CalendarRows;
        if (_viewModel.CalendarRows.Count > 0)
            CalendarList.Visibility = Visibility.Visible;
        AppDataSnapshot.SetEvents(_viewModel.Events.Select(e =>
            new AppDataSnapshot.EventSnapshot(e.Id, e.Summary,
                e.Start.ToString("o"), e.End.ToString("o"), e.RsvpStatus)));
        StartPoll();
    }

    /// <summary>
    /// Start the ~10s change-detection poll. Page_Loaded re-runs on every re-entry
    /// (incl. Back from EventDetailPage), so create the timer lazily and (re)start
    /// it here; OnNavigatedFrom stops it so it never leaks across pages. Timer
    /// create/start/stop must stay on the UI thread (here = UI thread).
    /// </summary>
    private void StartPoll()
    {
        _pollTimer ??= BuildPollTimer();
        if (!_pollTimer.IsEnabled) _pollTimer.Start();
    }

    private DispatcherTimer BuildPollTimer()
    {
        var timer = new DispatcherTimer { Interval = TimeSpan.FromSeconds(PollIntervalSecs) };
        timer.Tick += Poll_Tick;
        return timer;
    }

    private async void Poll_Tick(object? sender, object e)
    {
        if (_viewModel is null) return;
        // Don't race an explicit load (LoadAsync/Create/Rsvp/navigate set IsLoading);
        // skip this tick and let the next one re-check.
        if (_viewModel.IsLoading) return;

        EventsViewModel.EventsRefreshResult result;
        try
        {
            // No ConfigureAwait(false): off-thread bound-state mutation throws a
            // silent COMException on WinUI. The VM swallows transient RPC errors,
            // but guard here too against an unexpected throw.
            result = await _viewModel.RefreshIfChangedAsync();
        }
        catch
        {
            return;
        }

        // Steady-state poll = nothing changed = UI no-op. Bail before any rebuild
        // so an idle interval never churns the grid or the e2e snapshot.
        if (!result.AnyChanged) return;

        // The agenda (AgendaPanel.ItemsSource = Events) auto-updates on the
        // Events collection change. The month/week/day grids are imperatively
        // built and only re-run on a ViewMode change, so re-run the ACTIVE grid
        // builder with the refreshed events/colors (mirrors linux refresh_*_grid).
        if (_viewModel.ViewMode == FfiCalendarViewMode.Month) BuildMonthGrid();
        else if (_viewModel.ViewMode == FfiCalendarViewMode.Week) BuildWeekGrid();
        else if (_viewModel.ViewMode == FfiCalendarViewMode.Day) BuildDayTimeline();

        // Refresh the e2e snapshot off the now-current Events (same projection as
        // Page_Loaded) so the state protocol sees the externally-added event.
        AppDataSnapshot.SetEvents(_viewModel.Events.Select(ev =>
            new AppDataSnapshot.EventSnapshot(ev.Id, ev.Summary,
                ev.Start.ToString("o"), ev.End.ToString("o"), ev.RsvpStatus)));
    }

    protected override void OnNavigatedFrom(NavigationEventArgs e)
    {
        // Stop the poll while we're off the page (the page is cached —
        // NavigationCacheMode.Required — so the timer must pause while in
        // EventDetailPage and resume via Page_Loaded on Back; never leak it
        // across pages). Stop on the UI thread (here = UI thread).
        _pollTimer?.Stop();
        // Same reasoning for the deferred day-cell drill-in: a tick that lands
        // after we have navigated away would switch the view mode under a page
        // the user has left (this page is cached, so they would come back to it).
        _drillInTimer?.Stop();
        _dayCellClicks.OnDrillInDue();
        // The new-event form is modal (events.md § Persistence), so leaving the page
        // closes it, as it does on tui and web. Its half-written event is NOT lost: the
        // boxes are the live draft and this cached page keeps them, so the New Event
        // opener shows it again. Before this, a cached page brought the form back
        // still open, and no way out of it closed it except the toggle itself.
        EventCreationPanel.Visibility = Visibility.Collapsed;
        base.OnNavigatedFrom(e);
    }

    private void ViewModel_PropertyChanged(object? sender, System.ComponentModel.PropertyChangedEventArgs e)
    {
        if (_viewModel is null) return;
        switch (e.PropertyName)
        {
            case nameof(EventsViewModel.IsLoading):
                LoadingRing.IsActive = _viewModel.IsLoading;
                LoadingRing.Visibility = _viewModel.IsLoading ? Visibility.Visible : Visibility.Collapsed;
                break;
            case nameof(EventsViewModel.ErrorMessage):
                if (_viewModel.ErrorMessage is not null)
                {
                    ErrorBar.Message = _viewModel.ErrorMessage;
                    ErrorBar.IsOpen = true;
                    App.CurrentErrorMessage = _viewModel.ErrorMessage;
                }
                else { ErrorBar.IsOpen = false; App.CurrentErrorMessage = null; }
                break;
            case nameof(EventsViewModel.MonthYearDisplay):
                MonthYearText.Text = _viewModel.MonthYearDisplay;
                break;
            case nameof(EventsViewModel.ViewMode):
                AgendaPanel.Visibility = _viewModel.ViewMode == FfiCalendarViewMode.Agenda ? Visibility.Visible : Visibility.Collapsed;
                MonthGrid.Visibility = _viewModel.ViewMode == FfiCalendarViewMode.Month ? Visibility.Visible : Visibility.Collapsed;
                WeekGrid.Visibility = _viewModel.ViewMode == FfiCalendarViewMode.Week ? Visibility.Visible : Visibility.Collapsed;
                DayTimeline.Visibility = _viewModel.ViewMode == FfiCalendarViewMode.Day ? Visibility.Visible : Visibility.Collapsed;
                if (_viewModel.ViewMode == FfiCalendarViewMode.Month) BuildMonthGrid();
                else if (_viewModel.ViewMode == FfiCalendarViewMode.Week) BuildWeekGrid();
                else if (_viewModel.ViewMode == FfiCalendarViewMode.Day) BuildDayTimeline();   // body added in Task 5
                break;
            case nameof(EventsViewModel.ComposeStartPrefill):
                // Outlook double-click-empty-day → open the new-event compose with
                // its start prefilled to the cell's date (events.md § User actions;
                // EventsViewModel.NewEventOnDay sets the prefill). Consume the
                // one-shot signal (reset to null) so a later double-click on the
                // same date still re-fires PropertyChanged and re-opens the panel.
                if (_viewModel.ComposeStartPrefill is { } prefill)
                {
                    // A day-cell gesture STARTS FRESH (events.md § Persistence's
                    // second resume rule) — clear every field and the rail itself
                    // before applying the date prefill, not just the start box:
                    // otherwise a half-typed summary/description/location from a
                    // previous New Event opening would survive into what the user
                    // sees as a brand-new event.
                    EventSummaryBox.Text = "";
                    EventStartBox.Text = "";
                    EventEndBox.Text = "";
                    EventDescriptionBox.Text = "";
                    EventLocationBox.Text = "";
                    _ = App.EventDrafts?.ClearAsync();
                    EventStartBox.Text = prefill;
                    EventCreationPanel.Visibility = Visibility.Visible;
                    _viewModel.ComposeStartPrefill = null;
                }
                break;
        }
    }

    // `events-prev-month` / `events-next-month` — the sign only; the DISTANCE is
    // the shared pan policy, read per-click from `calendar_pan_step` inside
    // NavigateAsync (events.md § Where logic lives → *View mode + visible
    // range*). These used to pass ±1 to a month-only command, which stepped a
    // whole month in every view: ~4 weeks per click in week view, ~30 days in
    // day view, and an anchor move the date-unfiltered agenda ignores entirely.
    private async void PrevMonth_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.NavigateCommand.ExecuteAsync(-1);
    }

    private async void NextMonth_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        await _viewModel.NavigateCommand.ExecuteAsync(1);
    }

    private async void RsvpGoing_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not string id) return;
        await _viewModel.RsvpCommand.ExecuteAsync($"{id}:going");
    }

    private async void RsvpInterested_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not string id) return;
        await _viewModel.RsvpCommand.ExecuteAsync($"{id}:interested");
    }

    private async void RsvpDecline_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null || sender is not Button btn || btn.Tag is not string id) return;
        await _viewModel.RsvpCommand.ExecuteAsync($"{id}:declined");
    }

    private void AgendaView_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        _viewModel.ViewMode = FfiCalendarViewMode.Agenda;
    }

    private void MonthView_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        _viewModel.ViewMode = FfiCalendarViewMode.Month;
    }

    private void WeekView_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        _viewModel.ViewMode = FfiCalendarViewMode.Week;
    }

    private void DayView_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        _viewModel.ViewMode = FfiCalendarViewMode.Day;
    }

    private async void CalendarList_SelectionChanged(object sender, SelectionChangedEventArgs e)
    {
        if (_viewModel is null || CalendarList.SelectedItem is not CalendarRowVm row) return;
        // Record the selection so event create/query scope to it, then reload.
        _viewModel.SelectedCalendar = row.Calendar;
        try
        {
            await _viewModel.LoadCommand.ExecuteAsync(null);
            AgendaPanel.ItemsSource = _viewModel.Events;
        }
        catch (System.Exception ex)
        {
            ShellLog.Debug("EventsPage",
                $"selection reload failed: {ex.Message}");
        }
    }

    private void NewCalendar_Click(object sender, RoutedEventArgs e)
    {
        CalendarCreationPanel.Visibility =
            CalendarCreationPanel.Visibility == Visibility.Visible
                ? Visibility.Collapsed : Visibility.Visible;
    }

    private async void CreateCalendar_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        var name = CalendarNameBox.Text?.Trim();
        if (string.IsNullOrEmpty(name)) return;
        try
        {
            await _viewModel.CreateCalendarCommand.ExecuteAsync(name);
            CalendarCreationPanel.Visibility = Visibility.Collapsed;
            CalendarNameBox.Text = "";
        }
        catch (Exception ex)
        {
            ErrorBar.Message = Strings.Error(ex);
            ErrorBar.IsOpen = true;
            App.CurrentErrorMessage = ErrorBar.Message;
        }
    }

    private void NewEvent_Click(object sender, RoutedEventArgs e)
    {
        EventCreationPanel.Visibility =
            EventCreationPanel.Visibility == Visibility.Visible
                ? Visibility.Collapsed : Visibility.Visible;
    }

    private async void CreateEventInline_Click(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        var summary = EventSummaryBox.Text?.Trim();
        if (string.IsNullOrEmpty(summary)) return;
        try
        {
            // event-dtstart/event-dtend are combined date+time text inputs (ui.yaml).
            // Empty → sensible default (now / now+1h); non-empty but unparseable →
            // surface an error rather than silently create a wrong-dated event.
            if (!EventTimeUtil.TryParseEventDateTime(EventStartBox.Text, DateTimeOffset.Now, out var startDate))
            {
                ShowError(S.Get("events/invalid_datetime"));
                return;
            }
            if (!EventTimeUtil.TryParseEventDateTime(EventEndBox.Text, startDate.AddHours(1), out var endDate))
            {
                ShowError(S.Get("events/invalid_datetime"));
                return;
            }
            var draft = new EventDraft(
                summary,
                startDate.ToString("o"),
                endDate.ToString("o"),
                EventDescriptionBox.Text ?? "",
                EventLocationBox.Text ?? "");
            await _viewModel.CreateEventCommand.ExecuteAsync(draft);
            EventCreationPanel.Visibility = Visibility.Collapsed;
            EventSummaryBox.Text = "";
            EventStartBox.Text = "";
            EventEndBox.Text = "";
            EventDescriptionBox.Text = "";
            EventLocationBox.Text = "";
            // A successful create clears the rail (events.md § Persistence's third
            // resume rule) — here, on the SUCCESS path, never at the submit click: a
            // failed create (the catch block below) must leave the user's text where
            // they can retry it, which is exactly why this sits after ExecuteAsync
            // rather than before it.
            _ = App.EventDrafts?.ClearAsync();
        }
        catch (Exception ex)
        {
            ShowError(Strings.Error(ex));
        }
    }

    private void ShowError(string message)
    {
        ErrorBar.Message = message;
        ErrorBar.IsOpen = true;
        App.CurrentErrorMessage = ErrorBar.Message;
    }

    private void EventCard_Click(object sender, RoutedEventArgs e)
    {
        // Open the event detail page (canonical card → event_detail flow, matching
        // web/macOS/iOS/Linux). Delete + RSVP live on the detail surface there;
        // the inline RSVP buttons remain on the card as a quick affordance.
        if (_clients is null || sender is not Button btn
            || btn.Tag is not FaunaApp.Core.Models.EventInfo ev) return;
        Frame.Navigate(typeof(EventDetailPage),
            new EventDetailNavigationArgs(_clients, ev.Id, ev.RsvpStatus));
    }

    /// <summary>
    /// Flip a calendar's `calendar-visibility` display-filter checkbox
    /// (events.md § Where logic lives → Which calendars display). Checked/Unchecked
    /// (not Click): the e2e bridge actuates a WinUI CheckBox via its UIA
    /// TogglePattern, which flips IsChecked and raises Checked/Unchecked but never
    /// Click (mirrors AdminDnsPage's AutoRenew_Toggled). The x:Bind OneWay binding
    /// also re-fires Checked/Unchecked on every render/recycle, so ignore any
    /// change that merely echoes the row's current IsVisible — act only on a
    /// genuine flip.
    /// </summary>
    private void CalendarVisibility_Toggled(object sender, RoutedEventArgs e)
    {
        if (_viewModel is null) return;
        if (sender is not CheckBox cb || cb.DataContext is not CalendarRowVm row) return;
        var isChecked = cb.IsChecked == true;
        if (isChecked == row.IsVisible) return; // render-time binding echo, not a user action
        _viewModel.ToggleCalendarVisibility(row.Calendar.Id);
        if (_viewModel.ViewMode == FfiCalendarViewMode.Month) BuildMonthGrid();
        else if (_viewModel.ViewMode == FfiCalendarViewMode.Week) BuildWeekGrid();
        else if (_viewModel.ViewMode == FfiCalendarViewMode.Day) BuildDayTimeline();
    }

    private void DayCell_Click(object sender, RoutedEventArgs e)
    {
        // Single-click a month-grid day cell → Day view for that date (Outlook
        // month→day drill-in). The view-mode visibility wiring in
        // ViewModel_PropertyChanged toggles DayTimeline on ViewMode == "day".
        //
        // DEFERRED by the user's double-click time, because the drill-in
        // collapses MonthGrid and this cell is also the target of the
        // double-click compose gesture: acting here immediately unmounts the
        // cell before the second half of a double can reach it, which is
        // precisely how that gesture became unreachable for a real user (the
        // second physical click landed on the Day timeline's empty time slot
        // underneath and quick-created at ITS time). Deferring is the shape
        // apple already ships — SwiftUI resolves the same ambiguity implicitly
        // for FaunaKit's MonthGridView. Callers absorb the delay with a deadline
        // wait, never a sleep (actions/events.py::click_day_cell waits for
        // calendar-day-timeline).
        if (_viewModel is null || sender is not Button btn || btn.Tag is not DateTimeOffset date) return;
        if (!_dayCellClicks.OnClick(date, DateTime.UtcNow)) return;

        _drillInTimer ??= BuildDrillInTimer();
        _drillInTimer.Stop();   // re-arm: a click on another cell retargets it
        _drillInTimer.Start();
    }

    private DispatcherTimer BuildDrillInTimer()
    {
        var timer = new DispatcherTimer
        {
            Interval = TimeSpan.FromMilliseconds(GetDoubleClickTime()),
        };
        timer.Tick += (_, _) =>
        {
            _drillInTimer?.Stop();
            // Null when a double click already claimed this cell — the arbiter,
            // not the Stop() above, is what makes an already-queued tick safe.
            if (_dayCellClicks.OnDrillInDue() is { } due) _viewModel?.SelectDay(due);
        };
        return timer;
    }

    private void DayCell_DoubleTapped(object sender, Microsoft.UI.Xaml.Input.DoubleTappedRoutedEventArgs e)
    {
        // Double-click a month-grid day cell → open the new-event compose
        // prefilled with that date (Outlook's double-click-empty-day → new
        // event; events.md § User actions). The VM holds the prefill;
        // ViewModel_PropertyChanged opens the panel + fills EventStartBox.
        //
        // Cancels the pending drill-in so exactly ONE of the cell's two
        // gestures takes effect — § Layout & flow lists them as alternatives.
        if (_viewModel is null || sender is not Button btn || btn.Tag is not DateTimeOffset date) return;
        _drillInTimer?.Stop();
        _viewModel.NewEventOnDay(_dayCellClicks.OnDoubleClick(date, DateTime.UtcNow));
    }

    // ── Week/day time-grid helpers ────────────────────────────────────────────

    /// <summary>
    /// One day column: a Grid with Margin-positioned event-block Buttons.
    /// Uses Grid (not Canvas) so all children appear in the WinUI 3 UIA tree
    /// and are accessible to FlaUI — Canvas children are not UIA-traversable in
    /// WinUI 3 desktop apps. Reused by BuildWeekGrid (Task 4) and
    /// BuildDayTimeline (Task 5).
    /// </summary>
    private Grid BuildDayColumn(DateOnly date)
    {
        // A Grid with no RowDefinitions/ColumnDefinitions acts like an absolute-
        // layout panel: all children overlap, positioned via Margin. This is the
        // WinUI 3 UIA-safe replacement for Canvas absolute positioning.
        // AutomationId + Name on intermediate layout Grids: without both, WinUI 3's UIA
        // peer marks the element "raw" → FlaUI FindAllDescendants skips it and misses
        // all descendants (the "bare layout Grid prune trap").
        var grid = new Grid { Height = ColumnHeightPx, MinWidth = 60 };
        AutomationProperties.SetAutomationId(grid, $"calendar-day-col-{date:yyyy-MM-dd}");
        AutomationProperties.SetName(grid, $"Day {date:ddd d}");
        grid.HorizontalAlignment = HorizontalAlignment.Stretch;
        var layout = TimeGridLayout.ForDay(date, _viewModel!.Events);

        // Empty-slot quick-create markers FIRST, so the event blocks added below
        // land on top of them (see AddTimeSlotMarkers for why that ordering is
        // the whole hit-test story).
        AddTimeSlotMarkers(grid, date);

        foreach (var block in layout.Timed)
        {
            var topPx = block.StartMinutes / 30.0 * HalfHourPx;
            var heightPx = Math.Max(block.DurationMinutes / 30.0 * HalfHourPx, HalfHourPx);
            var btn = new Button
            {
                Content = new TextBlock
                {
                    Text = block.Event.Summary,
                    FontSize = 11,
                    TextTrimming = TextTrimming.CharacterEllipsis,
                    MaxLines = 2,
                },
                Tag = block.Event,
                Padding = new Thickness(2),
                HorizontalContentAlignment = HorizontalAlignment.Stretch,
                VerticalContentAlignment = VerticalAlignment.Top,
                // Stretch, so Place's left/right margins size the block to its
                // clash column. A Button defaults to Left, which sized every block
                // to its own text: the offsets packed side by side while the
                // widths ignored the column (measured by the timeline witness's
                // windows arm — six "columns" for a clash of two).
                HorizontalAlignment = HorizontalAlignment.Stretch,
                VerticalAlignment = VerticalAlignment.Top,
                Background = ColorFor(block.Event.CalendarColor),
                Height = heightPx,
            };
            // AutomationId shared across all blocks (enables count("calendar-event-block"));
            // Name is per-block unique (event summary) so FlaUI realizes it in the UIA tree.
            AutomationProperties.SetAutomationId(btn, Ids.CalendarEventBlock);
            AutomationProperties.SetName(btn, block.Event.Summary);   // required or FlaUI counts 0
            btn.Click += EventCard_Click;   // same card → event_detail nav as the agenda list

            // Position via Margin.Top (vertical) and Margin.Left (horizontal fraction of column).
            // Margin.Right is set to leave the right portion for sibling columns in overlap.
            void Place(double w)
            {
                double colW = w / block.ColumnCount;
                double left = block.ColumnIndex * colW;
                double right = w - left - Math.Max(colW - 2, 1);
                btn.Margin = new Thickness(left, topPx, Math.Max(right, 0), 0);
            }
            grid.SizeChanged += (_, e2) => Place(e2.NewSize.Width);
            if (grid.ActualWidth > 0) Place(grid.ActualWidth);
            grid.Children.Add(btn);
        }
        return grid;
    }

    /// <summary>
    /// Tile one 15-minute empty-slot quick-create marker per slot across the 24h
    /// column, each carrying the indexed ui.yaml id
    /// <c>events-time-slot-{HH-MM}</c> and opening the new-event compose
    /// prefilled with this column's date AND its own snapped time (events.md
    /// § Week &amp; day timeline views). Mirrors linux's reference leg
    /// (<c>week_grid.rs::add_time_slot_markers</c>): all 96 slots tiled, whether
    /// or not an event sits over them.
    ///
    /// Three things this shape gets right, each of which bit another app:
    ///
    /// 1. The slot's time is CLOSURE-CAPTURED from the marker, never derived
    ///    from pointer y. A headless driver's click carries no meaningful
    ///    coordinates, so a y-computed time would always have read 00:00 — and
    ///    apple's leg shipped the tap target but hardcoded 09:00 for want of
    ///    exactly this.
    /// 2. Markers are added to the column BEFORE the event blocks, so WinUI hit
    ///    tests the later-added blocks first and a real mouse click on a block
    ///    opens the event rather than quick-creating under it (ui.yaml
    ///    <c>events-time-slot</c>: "a click landing on an event block targets the
    ///    block, never the slot under it"). This is structural — no gesture
    ///    ordering to get wrong, which is precisely the bug linux carried until
    ///    2026-08-01 with its column-level background gesture. The e2e driver
    ///    never relies on it: the FlaUI bridge invokes a slot by id via
    ///    InvokePattern, which is z-order-blind by construction.
    /// 3. AutomationId AND Name on every marker — without a Name, WinUI 3 marks
    ///    the peer "raw" and FlaUI's FindAllDescendants prunes it (the same
    ///    bare-layout trap documented on the day-column Grid above).
    ///
    /// Cost of the 96 × 7 = 672 markers a week grid carries in a non-virtualizing
    /// Grid (weighed before committing, per the leg's own brief): each marker is a
    /// content-less, border-less, non-tab-stop Button — one ContentPresenter and
    /// one UIA node apiece, no per-marker SizeChanged handler (unlike the blocks,
    /// they span the full column width and need no re-placement). The UIA tree is
    /// what actually grows, and the alternative that would shrink it — tiling only
    /// the slots no event covers, as tui does — saves nothing on a normal calendar
    /// day (a handful of events cover a handful of slots) while costing the
    /// property in (2) and diverging from the pixel-grid reference leg.
    /// </summary>
    private void AddTimeSlotMarkers(Grid column, DateOnly date)
    {
        var columnDate = new DateTimeOffset(date.ToDateTime(TimeOnly.MinValue));
        foreach (var slot in TimeGridLayout.QuarterHourSlots())
        {
            var marker = new Button
            {
                Content = null,
                Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Transparent),
                BorderThickness = new Thickness(0),
                Padding = new Thickness(0),
                MinWidth = 0,
                MinHeight = 0,
                Height = QuarterHourPx,
                // 672 focusable empty slots would swamp keyboard navigation of the
                // week grid; linux's markers are likewise gesture-only, not focusable.
                IsTabStop = false,
                HorizontalAlignment = HorizontalAlignment.Stretch,
                VerticalAlignment = VerticalAlignment.Top,
                Margin = new Thickness(0, slot.StartMinutes / 30.0 * HalfHourPx, 0, 0),
            };
            AutomationProperties.SetAutomationId(marker, slot.ElementId);
            AutomationProperties.SetName(marker, slot.Label);
            // `slot` and `columnDate` are captured per iteration (foreach), so each
            // marker carries its own time — the point of (1) above.
            marker.Click += (_, _) => _viewModel?.NewEventAtSlot(columnDate, slot.Hour, slot.Minute);
            column.Children.Add(marker);
        }
    }

    // HexToColorConverter.Convert always returns a SolidColorBrush (it falls back to
    // CornflowerBlue internally for a missing/short hex), so this cast is total — no
    // second ?? fallback is reachable.
    private static Microsoft.UI.Xaml.Media.Brush ColorFor(string? hex) =>
        (Microsoft.UI.Xaml.Media.Brush)new HexToColorConverter().Convert(hex ?? "", typeof(object), null!, "");

    /// <summary>
    /// Wrap a time-grid body (gutter + day columns) in a vertically-scrolling
    /// ScrollViewer that auto-scrolls to ~08:00 on first layout (events.md
    /// § Week & day timeline views; mirrors linux week_grid/day_grid scrolling
    /// 8*2*HALF_HOUR_PX). Horizontal scroll is disabled so the body's star
    /// day-columns size to the viewport width. Blocks stay FlaUI-countable: the
    /// body is a non-virtualizing Grid, so every calendar-event-block keeps a
    /// realized UIA peer even when scrolled offscreen — virtualization, not the
    /// ScrollViewer, is what hides offscreen children (cf. FeedPage's
    /// non-virtualizing PostsList that counts every offscreen post-card).
    /// </summary>
    private ScrollViewer WrapInTimeScroll(UIElement body)
    {
        var scroll = new ScrollViewer
        {
            VerticalScrollMode = ScrollMode.Enabled,
            VerticalScrollBarVisibility = ScrollBarVisibility.Auto,
            HorizontalScrollMode = ScrollMode.Disabled,
            HorizontalScrollBarVisibility = ScrollBarVisibility.Disabled,
            Content = body,
        };
        // Defer the 8am scroll until the content has measured: ScrollableHeight
        // is 0 until first layout, so a ChangeView in Loaded would be a no-op.
        // A one-shot LayoutUpdated fires repeatedly (cheap no-op) until layout
        // completes, then scrolls once and unsubscribes.
        void OnLayout(object? s, object e)
        {
            if (scroll.ScrollableHeight <= 0) return;
            var target = Math.Min(8 * 2 * HalfHourPx, scroll.ScrollableHeight);
            scroll.ChangeView(null, target, null, disableAnimation: true);
            scroll.LayoutUpdated -= OnLayout;
        }
        scroll.LayoutUpdated += OnLayout;
        return scroll;
    }

    /// <summary>Hour-gutter column (00:00..23:00).</summary>
    private static StackPanel BuildHourGutter()
    {
        var col = new StackPanel { Width = HourGutterWidth };
        for (int h = 0; h < 24; h++)
            col.Children.Add(new TextBlock
            {
                Text = $"{h:00}:00",
                FontSize = 11,
                Opacity = 0.6,
                Height = 2 * HalfHourPx,
                HorizontalAlignment = HorizontalAlignment.Right,
                Margin = new Thickness(0, 0, 4, 0),
            });
        return col;
    }

    /// <summary>All-day band: id-bearing container + per-day all-day chips.</summary>
    private Border BuildAllDayBand(IReadOnlyList<DateOnly> days)
    {
        var grid = new Grid { MinHeight = 24 };
        grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(HourGutterWidth) });
        for (int c = 0; c < days.Count; c++)
            grid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var lead = new TextBlock
        {
            Text = "all-day",
            FontSize = 10,
            Opacity = 0.6,
            HorizontalAlignment = HorizontalAlignment.Right,
            Margin = new Thickness(0, 0, 4, 0),
        };
        Grid.SetColumn(lead, 0);
        grid.Children.Add(lead);

        for (int c = 0; c < days.Count; c++)
        {
            var stack = new StackPanel { Spacing = 1 };
            foreach (var ev in TimeGridLayout.ForDay(days[c], _viewModel!.Events).AllDay)
                stack.Children.Add(new TextBlock
                {
                    Text = ev.Summary,
                    FontSize = 10,
                    TextTrimming = TextTrimming.CharacterEllipsis,
                    MaxLines = 1,
                });
            Grid.SetColumn(stack, c + 1);
            grid.Children.Add(stack);
        }

        var border = new Border { Child = grid };
        AutomationProperties.SetAutomationId(border, Ids.CalendarAlldayBand);
        AutomationProperties.SetName(border, "All-day events");   // required or FlaUI prunes it
        return border;
    }

    /// <summary>Current-time line overlaid on a day column (today only).</summary>
    private static void AddCurrentTimeLine(Grid dayCol)
    {
        var now = DateTime.Now;
        double topPx = (now.Hour * 60 + now.Minute) / 30.0 * HalfHourPx;
        var line = new Border
        {
            Height = 2,
            HorizontalAlignment = HorizontalAlignment.Stretch,
            VerticalAlignment = VerticalAlignment.Top,
            Margin = new Thickness(0, topPx, 0, 0),
            Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Red),
        };
        AutomationProperties.SetAutomationId(line, Ids.CalendarCurrentTime);
        AutomationProperties.SetName(line, "Current time");   // required or FlaUI prunes it
        dayCol.Children.Add(line);
    }

    private void BuildWeekGrid()
    {
        WeekGrid.Children.Clear();
        WeekGrid.RowDefinitions.Clear();
        WeekGrid.ColumnDefinitions.Clear();

        WeekGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });    // header
        WeekGrid.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto });    // all-day band
        WeekGrid.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) }); // time grid

        // One probe, one rotation — the same FaunaApp.Core.Calendar.WeekStart the
        // month grid and the week range label now use. This site already had the
        // correct arithmetic while the month grid hardcoded Sunday-first; sharing
        // it is what stops the two disagreeing again.
        var sel = _viewModel!.CurrentDay.Date;
        var weekStartDate = WeekStart.StartOfWeek(sel, WeekStart.Current).Date;
        var days = Enumerable.Range(0, 7)
            .Select(i => DateOnly.FromDateTime(weekStartDate.AddDays(i)))
            .ToList();
        var today = DateOnly.FromDateTime(DateTime.Now);

        // Header row: gutter spacer + 7 day labels.
        var header = new Grid();
        header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(HourGutterWidth) });
        for (int c = 0; c < 7; c++)
            header.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });
        for (int c = 0; c < 7; c++)
        {
            var d = days[c];
            var lbl = new TextBlock
            {
                Text = $"{ValueFormat.WeekdayAbbreviation(d.DayOfWeek)} {d.Day}",
                HorizontalAlignment = HorizontalAlignment.Center,
                FontSize = 12,
                FontWeight = d == today
                    ? Microsoft.UI.Text.FontWeights.Bold
                    : Microsoft.UI.Text.FontWeights.Normal,
            };
            Grid.SetColumn(lbl, c + 1);
            header.Children.Add(lbl);
        }
        Grid.SetRow(header, 0);
        WeekGrid.Children.Add(header);

        // All-day band (always present so its id realizes; shows chips when any).
        var allDayBand = BuildAllDayBand(days);
        Grid.SetRow(allDayBand, 1);
        WeekGrid.Children.Add(allDayBand);

        // Time grid: gutter + 7 day columns.
        // The body Grid uses Grid (not Canvas) day columns so event-block Buttons
        // appear in the WinUI 3 UIA tree and are accessible to FlaUI — Canvas
        // children are NOT UIA-traversable in WinUI 3 desktop apps; Grid children
        // ARE. The body is then wrapped (WrapInTimeScroll) in a vertical-only
        // ScrollViewer so the full 24h (1440px) grid is scrollable and auto-scrolls
        // to ~08:00 on first layout (events.md § Week & day timeline views). The
        // non-virtualizing Grid keeps every offscreen calendar-event-block realized,
        // so FlaUI still counts blocks scrolled out of view.
        var body = new Grid();
        // AutomationId + Name on intermediate layout Grids: without both, WinUI 3's UIA peer
        // marks the element "raw" which causes FlaUI's FindAllDescendants to skip it and miss
        // all descendants — the same "bare layout Grid prune trap" documented in the XAML comment
        // above MonthGrid.
        AutomationProperties.SetAutomationId(body, "calendar-week-body");
        AutomationProperties.SetName(body, "Week body");
        body.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(HourGutterWidth) });
        for (int c = 0; c < 7; c++)
            body.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var gutter = BuildHourGutter();
        Grid.SetColumn(gutter, 0);
        body.Children.Add(gutter);

        for (int c = 0; c < 7; c++)
        {
            var dayCol = BuildDayColumn(days[c]);
            Grid.SetColumn(dayCol, c + 1);
            if (days[c] == today) AddCurrentTimeLine(dayCol);
            body.Children.Add(dayCol);
        }

        var scroll = WrapInTimeScroll(body);
        Grid.SetRow(scroll, 2);
        WeekGrid.Children.Add(scroll);
    }

    /// <summary>Day timeline (Outlook-style single-day view). Body implemented in Task 5.</summary>
    private void BuildDayTimeline()
    {
        DayTimeline.Children.Clear();
        DayTimeline.RowDefinitions.Clear();
        DayTimeline.RowDefinitions.Add(new RowDefinition { Height = GridLength.Auto }); // all-day band
        DayTimeline.RowDefinitions.Add(new RowDefinition { Height = new GridLength(1, GridUnitType.Star) }); // scrollable body

        var day = DateOnly.FromDateTime(_viewModel!.CurrentDay.Date);
        var today = DateOnly.FromDateTime(DateTime.Now);

        // All-day band
        var allDay = BuildAllDayBand(new[] { day });
        Grid.SetRow(allDay, 0);
        DayTimeline.Children.Add(allDay);

        // Time grid: hour gutter + single day column. The body Grid is wrapped
        // (WrapInTimeScroll) in a vertical-only ScrollViewer so the full 24h
        // (1440px) grid is scrollable and auto-scrolls to ~08:00 on first layout
        // (events.md § Week & day timeline views). The non-virtualizing Grid keeps
        // every offscreen calendar-event-block realized, so FlaUI still counts
        // blocks scrolled out of view. The body Grid is given explicit AutomationId
        // + Name per the "bare layout Grid prune trap" rule (without both WinUI 3
        // marks the Grid raw and FlaUI skips its descendants entirely).
        var body = new Grid();
        AutomationProperties.SetAutomationId(body, "calendar-day-body");
        AutomationProperties.SetName(body, "Day body");
        body.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(HourGutterWidth) });
        body.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        var gutter = BuildHourGutter();
        Grid.SetColumn(gutter, 0);
        body.Children.Add(gutter);

        var col = BuildDayColumn(day);
        Grid.SetColumn(col, 1);
        if (day == today) AddCurrentTimeLine(col);
        body.Children.Add(col);

        var scroll = WrapInTimeScroll(body);
        Grid.SetRow(scroll, 1);
        DayTimeline.Children.Add(scroll);
    }

    private void BuildMonthGrid()
    {
        MonthGrid.Children.Clear();
        MonthGrid.RowDefinitions.Clear();
        MonthGrid.ColumnDefinitions.Clear();

        // 7 columns
        for (int c = 0; c < 7; c++)
            MonthGrid.ColumnDefinitions.Add(new ColumnDefinition { Width = new GridLength(1, GridUnitType.Star) });

        // Header row + 6 week rows
        for (int r = 0; r < 7; r++)
            MonthGrid.RowDefinitions.Add(new RowDefinition { Height = r == 0 ? GridLength.Auto : new GridLength(1, GridUnitType.Star) });

        // Day names header. Column ORDER now follows the locale's first day of the
        // week (events.md § Week & day timeline views — windows' mechanism is
        // CultureInfo.FirstDayOfWeek), the same probe the week grid and the week
        // range label use, through the one FaunaApp.Core.Calendar.WeekStart
        // helper. It was hardcoded Sunday-first until 2026-08-24, header and cell
        // placement both, so in a Monday-first locale this grid and the week grid
        // disagreed about which column a date sits in. The label TEXT stays
        // localized through the i18n-catalog-backed helper (events.md: localized
        // weekday names are per-app i18n, not fauna_core).
        var weekStart = WeekStart.Current;
        for (int c = 0; c < 7; c++)
        {
            var tb = new TextBlock { Text = ValueFormat.WeekdayAbbreviation(WeekStart.WeekdayInColumn(c, weekStart)), HorizontalAlignment = HorizontalAlignment.Center, Opacity = 0.6, FontSize = 12 };
            Grid.SetRow(tb, 0);
            Grid.SetColumn(tb, c);
            MonthGrid.Children.Add(tb);
        }

        // Calculate first day of month
        var month = _viewModel!.CurrentMonth;
        var firstDay = new DateTime(month.Year, month.Month, 1);
        var daysInMonth = DateTime.DaysInMonth(month.Year, month.Month);
        // The month's first day, placed in the column the LOCALE puts it in —
        // not `(int)firstDay.DayOfWeek`, which is Sunday-first by definition.
        var startCol = WeekStart.ColumnOf(firstDay.DayOfWeek, weekStart);

        // Fill day cells
        for (int day = 1; day <= daysInMonth; day++)
        {
            var row = (startCol + day - 1) / 7 + 1;
            var col = (startCol + day - 1) % 7;

            var cellPanel = new StackPanel { Padding = new Thickness(2) };
            var dayText = new TextBlock { Text = day.ToString(), FontWeight = Microsoft.UI.Text.FontWeights.SemiBold };
            cellPanel.Children.Add(dayText);

            // Show up to 2 events for this day
            var dayDate = new DateTime(month.Year, month.Month, day);
            var dayEvents = _viewModel.Events.Where(e => e.Start.Date == dayDate).Take(2);
            foreach (var evt in dayEvents)
            {
                var evtText = new TextBlock { Text = evt.Summary, FontSize = 10, TextTrimming = TextTrimming.CharacterEllipsis, MaxLines = 1, Opacity = 0.7 };
                cellPanel.Children.Add(evtText);
            }

            // Make the cell a clickable, flat, full-cell Button → single-click
            // drills into Day view for this date (Microsoft Outlook's month→day,
            // ui.yaml `events-day-cell-{YYYY-MM-DD}`). A Button is the most
            // reliable surface for FlaUI to Invoke; it must carry a non-empty
            // AutomationProperties.Name (FlaUI counts 0 without one) and stay
            // always-enabled (a disabled WinUI button can't be Invoked). The
            // DateTimeOffset for SelectDay rides the Tag so the lambda captures
            // the per-cell date without re-deriving it.
            var dayOffset = new DateTimeOffset(dayDate);
            var cellButton = new Button
            {
                Content = cellPanel,
                Tag = dayOffset,
                Background = new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.Transparent),
                BorderThickness = new Thickness(0),
                Padding = new Thickness(0),
                HorizontalAlignment = HorizontalAlignment.Stretch,
                VerticalAlignment = VerticalAlignment.Stretch,
                HorizontalContentAlignment = HorizontalAlignment.Stretch,
                VerticalContentAlignment = VerticalAlignment.Top,
            };
            // AutomationProperties are attached properties — set via the static
            // setters, not an object initializer. The id is the ISO machine key
            // (events-day-cell-{iso}, read by has_day_cell/click_day_cell — the
            // e2e layer never reads Name's content, only its non-emptiness); the
            // Name is what a screen reader actually speaks, so it wants a real
            // date, not the ISO key repeated (value-formatting.md § Absolute
            // local timestamp display — this exact line was frozen there for a
            // Windows session to resolve; NOT the shared format_unix_local_date
            // door, since dayDate is a locally-computed calendar value with no
            // epoch/timezone origin — routing it through an epoch-based
            // converter would be an invented round-trip, not a de-duplication).
            // "D" is .NET's culture-aware long date pattern, matching the
            // project's established CultureInfo-driven windows date
            // localization (events.md § Week & day timeline views) and — unlike
            // BuildDayColumn's abbreviated sibling `$"Day {date:ddd d}"`
            // (line ~510, scoped to a known week) — spells month+day+year in
            // full, since a month-grid cell carries no such surrounding context.
            AutomationProperties.SetAutomationId(cellButton, $"events-day-cell-{dayDate:yyyy-MM-dd}");
            AutomationProperties.SetName(cellButton, dayDate.ToString("D"));
            cellButton.Click += DayCell_Click;
            // handledEventsToo: ButtonBase marks the pointer events it consumes
            // for Click as handled, so a plain `DoubleTapped +=` on a Button
            // never fires. This is the second half of why the double-click
            // gesture was unreachable (the first being the drill-in unmounting
            // the cell — see DayCell_Click).
            cellButton.AddHandler(
                UIElement.DoubleTappedEvent,
                new Microsoft.UI.Xaml.Input.DoubleTappedEventHandler(DayCell_DoubleTapped),
                handledEventsToo: true);

            Grid.SetRow(cellButton, row);
            Grid.SetColumn(cellButton, col);
            MonthGrid.Children.Add(cellButton);
        }
    }
}

public class HexToColorConverter : IValueConverter
{
    public object Convert(object value, Type targetType, object parameter, string language)
    {
        if (value is string hex && hex.StartsWith("#") && hex.Length >= 7)
        {
            var r = System.Convert.ToByte(hex.Substring(1, 2), 16);
            var g = System.Convert.ToByte(hex.Substring(3, 2), 16);
            var b = System.Convert.ToByte(hex.Substring(5, 2), 16);
            return new Microsoft.UI.Xaml.Media.SolidColorBrush(Windows.UI.Color.FromArgb(255, r, g, b));
        }
        return new Microsoft.UI.Xaml.Media.SolidColorBrush(Microsoft.UI.Colors.CornflowerBlue);
    }

    public object ConvertBack(object value, Type targetType, object parameter, string language) => throw new NotSupportedException();
}
