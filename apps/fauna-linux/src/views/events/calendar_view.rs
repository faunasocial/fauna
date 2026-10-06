use adw::prelude::*;
use fauna_core::caltime::{PanDirection, pan};
use fauna_ui_ids as ids;
use gtk::gdk;
use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::rc::Rc;

use super::calendar_sidebar;
use super::day_cell_press;
use super::day_grid;
use super::event_detail;
use super::event_list;
use super::event_popover;
use super::mini_month;
use super::month_grid;
use super::time_utils;
use super::week_grid;
use crate::client::FaunaClient;
use crate::i18n::strings::common;
use crate::i18n::strings::events as events_strings;
use crate::window_state;

// ---------------------------------------------------------------------------
// State
// ---------------------------------------------------------------------------

/// The active view is the shared [`fauna_core::caltime::CalendarViewMode`] —
/// one vocabulary and one pan policy across all 7 apps (`goal/ui/events.md`
/// § Where logic lives). Its `as_wire()` doubles as the GTK stack child name,
/// which is where linux's own `stack_name()` came from.
pub use fauna_core::caltime::CalendarViewMode as ViewMode;

/// Switch the page to `mode` showing `date` — the one way a view other than the
/// mode buttons changes what the page shows (the month→day drill-in, "+N more",
/// the keyboard shortcuts).
///
/// It exists because `view_mode` alone is inert: only `build_calendar_view` owns
/// `main_stack`, so a caller that sets `state.view_mode` and stops leaves state
/// claiming one view while the stack still shows another — the desync the
/// month grid's "+N more" shipped with. Handing views this closure rather than a
/// `set_active_view` setter on `CalendarViewState` is deliberate, and mirrors the
/// `open_profile` seam: a new construction site that forgets it fails to
/// *compile* instead of silently no-op'ing at runtime.
pub type ViewSwitch = Rc<dyn Fn(ViewMode, (i32, u32, u32))>;

/// What the month grid needs from the page it lives in. The first two members
/// exist for the same reason: only `build_calendar_view` owns the widgets
/// involved, and a grid that tried to reach them itself would either not
/// compile or silently do nothing.
#[derive(Clone)]
pub struct MonthGridHooks {
    /// Switch the page to a view/date — see [`ViewSwitch`].
    pub switch_view: ViewSwitch,
    /// Open the quick-create compose for a date, anchored somewhere that stays
    /// mapped. The month grid cannot anchor it to the clicked cell: the
    /// double-click's own drill-in unmaps that cell first, and a popover
    /// parented to an unmapped widget never appears (nor to the window — that
    /// leaves it out of the widget tree entirely, measured 2026-07-30).
    pub open_quick_create: Rc<dyn Fn((i32, u32, u32))>,
    /// Which of a cell's two gestures a press turns out to be
    /// ([`day_cell_press`]). One chain for the whole grid, not one per cell: a
    /// press at cell A followed by one at cell B is two single clicks, and only
    /// a shared chain can tell that from a double click at A.
    ///
    /// It outlives `refresh_month_grid`, which rebuilds every cell — a
    /// per-cell arbiter would be dropped mid-chain by the very repaint the
    /// deferral exists to survive.
    pub presses: Rc<RefCell<day_cell_press::DayCellPress>>,
}

/// How the today / prev / next header buttons move `selected_date`. The step
/// size itself is view-mode-dependent — one visible range per click, per the
/// shared `fauna_core::caltime::pan_step` policy.
#[derive(Clone, Copy)]
enum DateStep {
    Today,
    Backward,
    Forward,
}

pub struct CalendarViewState {
    pub selected_date: (i32, u32, u32),
    pub view_mode: ViewMode,
    pub visible_calendars: HashSet<String>,
    pub events_cache: Vec<crate::rows::EventRow>,
    pub calendar_colors: HashMap<String, String>,
    /// Cached attendees per event: event_id -> roster rows (name + email +
    /// projected RSVP), rendered by `event_detail::build_attendee_row`.
    pub attendees_cache: HashMap<String, Vec<super::caldav_backend::CalDavAttendee>>,
    /// Cached reminder offset per event: event_id (hex `uid_hash`) -> the
    /// ISO-8601 preset (e.g. `PT1H`) or `None` when no reminder is set.
    /// Populated by `EventReminderLoaded` (on select / set / remove); drives the
    /// two-state reminder control on the detail panel.
    pub reminders_cache: HashMap<String, Option<String>>,
    /// The event currently shown in the persistent detail panel (right column),
    /// or `None` when the panel shows its empty state. Clicking an event card in
    /// any view sets this; the panel re-renders reactively (observer-driven, per
    /// goal/ui/events.md arch rule #1) — including when `EventAttendeesLoaded`
    /// arrives for this event. Mirrors web/macOS, which render event detail in a
    /// stable reactive panel rather than a transient popover.
    pub selected_event: Option<crate::rows::EventRow>,
    /// The calendar list currently rendered in the sidebar. Lets the
    /// `CalendarsLoaded` handler skip the sidebar rebuild when a poll
    /// (app.rs § CALENDAR_POLL_INTERVAL_SECS) re-lists an unchanged set — so a
    /// steady-state poll never destroys/recreates `calendar-item` widgets under
    /// the user's cursor. Updated whenever the sidebar is actually rebuilt.
    pub displayed_calendars: Vec<crate::rows::CalendarRow>,
    /// The calendar picked by clicking a `calendar-item`, or `None` for the
    /// no-selection union (goal/ui/events.md § User actions: "Select calendar
    /// (filter visible events)"). Distinct from `visible_calendars`, which is
    /// the separate `calendar-visibility` toggle — see
    /// [`CalendarViewState::shows_calendar`] for how the two compose.
    pub selected_calendar: Option<String>,
}

impl CalendarViewState {
    /// The *live* calendar selection: `Some(id)` only while that calendar is
    /// still listed, else `None` (the union). The staleness rule is shared with
    /// tui — `fauna_client_caldav::resolve_calendar_selection` — so a calendar
    /// deleted here or by a CalDAV MUA can never strand the page on a blank
    /// list.
    pub fn live_selection(&self) -> Option<&str> {
        fauna_client_caldav::resolve_calendar_selection(
            self.selected_calendar.as_deref(),
            self.displayed_calendars.iter().map(|c| &c.id),
        )
    }

    /// Does an event on `calendar_id` belong on the page right now?
    ///
    /// The two sidebar affordances never fight, because only one is in force at
    /// a time: a live **selection** (`calendar-item`) narrows the page to that
    /// one calendar; with no selection the **visibility** checkboxes
    /// (`calendar-visibility`) filter the union (an empty set being the union
    /// itself, the pre-first-load state). This one predicate backs the agenda
    /// and all three grids — they filtered on `visible_calendars` directly and
    /// had already drifted apart on the empty-set case.
    ///
    /// The composition itself is now shared Rust —
    /// `fauna_client_caldav::calendar_is_displayed`, generalized from this very
    /// method 2026-08-02 and consumed by tui too, so the two apps cannot drift
    /// back apart (`goal/ui/events.md` § Where logic lives → *Which calendars
    /// display*). It takes the **raw** stored selection and resolves staleness
    /// internally, which is why this passes `selected_calendar` rather than
    /// [`Self::live_selection`].
    pub fn shows_calendar(&self, calendar_id: &str) -> bool {
        fauna_client_caldav::calendar_is_displayed(
            self.selected_calendar.as_deref(),
            self.displayed_calendars.iter().map(|c| &c.id),
            &self.visible_calendars,
            calendar_id,
        )
    }

    /// The calendar an authoring action targets — new event, `.ics` import,
    /// `.ics` export (goal/ui/events.md § Layout & flow: creating targets the
    /// active calendar; § Import / Export: both are scoped to "the currently
    /// selected calendar"). The live selection wins; with none, the first
    /// visible calendar, then simply the first listed.
    pub fn authoring_target(&self) -> Option<String> {
        if let Some(selected) = self.live_selection() {
            return Some(selected.to_string());
        }
        self.displayed_calendars
            .iter()
            .find(|c| self.visible_calendars.contains(&c.id))
            .or_else(|| self.displayed_calendars.first())
            .map(|c| c.id.clone())
    }
}

/// Group the cached events by `"YYYY-MM-DD"` start date, keeping only those in
/// the page's current calendar scope ([`CalendarViewState::shows_calendar`]).
///
/// One copy, shared by all three grids. There used to be three byte-identical
/// private copies (month/week/day) and they had already drifted from the
/// agenda's own filter on the empty-`visible_calendars` case: the grids read it
/// as "hide everything", the agenda as "the union".
pub fn index_events_by_date(
    state: &CalendarViewState,
) -> HashMap<String, Vec<crate::rows::EventRow>> {
    let mut map: HashMap<String, Vec<crate::rows::EventRow>> = HashMap::new();
    for ev in &state.events_cache {
        if !state.shows_calendar(&ev.calendar_id) {
            continue;
        }
        if let Some((ey, em, ed)) = time_utils::parse_date(&ev.start_time) {
            let key = format!("{:04}-{:02}-{:02}", ey, em, ed);
            map.entry(key).or_default().push(ev.clone());
        }
    }
    map
}

// ---------------------------------------------------------------------------
// Handles
// ---------------------------------------------------------------------------

#[allow(dead_code)]
pub struct CalendarViewHandles {
    pub state: Rc<RefCell<CalendarViewState>>,
    pub main_stack: gtk::Stack,
    pub date_label: gtk::Label,
    pub sidebar_box: gtk::Box,
    pub calendar_list_box: gtk::ListBox,
    pub month_grid_container: gtk::Box,
    pub week_grid_container: gtk::Box,
    pub day_grid_container: gtk::Box,
    pub agenda_container: gtk::Box,
    /// The persistent event-detail panel (right column). Re-rendered by
    /// `event_detail::refresh_event_detail_panel` on `EventSelected` /
    /// `EventAttendeesLoaded`.
    pub detail_panel: gtk::Box,
    /// The page callbacks `month_grid::refresh_month_grid` requires — its day
    /// cells wire them to the month→day drill-in and the double-click compose.
    pub month_hooks: MonthGridHooks,
}

// ---------------------------------------------------------------------------
// Date label updater
// ---------------------------------------------------------------------------

fn update_date_label(label: &gtk::Label, state: &CalendarViewState) {
    let (y, m, d) = state.selected_date;
    let text = match state.view_mode {
        ViewMode::Month => time_utils::format_month_label(y, m),
        ViewMode::Week => time_utils::format_week_label(y, m, d, time_utils::locale_week_start()),
        ViewMode::Day => time_utils::format_day_label(y, m, d),
        ViewMode::Agenda => crate::i18n::strings::common::UPCOMING.to_string(),
    };
    label.set_label(&text);
}

// ---------------------------------------------------------------------------
// Step helpers (month-aware navigation)
// ---------------------------------------------------------------------------

/// Step the selected date forward by one visible range — the shared
/// `caltime::pan` policy, so every app moves the same distance.
fn step_forward(state: &mut CalendarViewState) {
    state.selected_date = pan(state.view_mode, state.selected_date, PanDirection::Forward);
}

/// Step the selected date backward by one visible range.
fn step_backward(state: &mut CalendarViewState) {
    state.selected_date = pan(state.view_mode, state.selected_date, PanDirection::Backward);
}

// ---------------------------------------------------------------------------
// Builder
// ---------------------------------------------------------------------------

pub fn build_calendar_view(client: &Rc<FaunaClient>) -> (gtk::Box, CalendarViewHandles) {
    let today = time_utils::today();

    // Read persisted view mode from window state (falls back to Agenda, the
    // cross-app default events view — see window_state::default_calendar_mode).
    let initial_view_mode = window_state::load_window_state()
        .and_then(|ws| ViewMode::from_wire(&ws.calendar_view_mode))
        .unwrap_or_default();

    let state = Rc::new(RefCell::new(CalendarViewState {
        selected_date: today,
        view_mode: initial_view_mode,
        visible_calendars: HashSet::new(),
        events_cache: Vec::new(),
        calendar_colors: HashMap::new(),
        attendees_cache: HashMap::new(),
        reminders_cache: HashMap::new(),
        selected_event: None,
        displayed_calendars: Vec::new(),
        selected_calendar: None,
    }));

    // -----------------------------------------------------------------------
    // Left sidebar panel (240px)
    // -----------------------------------------------------------------------
    let sidebar_box = gtk::Box::new(gtk::Orientation::Vertical, 12);
    sidebar_box.set_width_request(240);
    sidebar_box.set_margin_top(12);
    sidebar_box.set_margin_bottom(12);
    sidebar_box.set_margin_start(12);
    sidebar_box.set_margin_end(12);

    // Calendar sidebar (color checkboxes + "New Calendar" button).
    let month_grid_container_for_sidebar = Rc::new(RefCell::new(None::<gtk::Box>));
    let week_grid_container_for_sidebar = Rc::new(RefCell::new(None::<gtk::Box>));
    let day_grid_container_for_sidebar = Rc::new(RefCell::new(None::<gtk::Box>));
    let agenda_container_for_sidebar = Rc::new(RefCell::new(None::<gtk::Box>));
    // Same late-bound slot as the containers above, for the same reason: these
    // two sidebar callbacks are wired before the stack — and so before
    // `switch_view` — exists. The month grid's *cells* take the switch as a
    // parameter instead, which is where the compile-time enforcement matters.
    let month_hooks_for_sidebar = Rc::new(RefCell::new(None::<MonthGridHooks>));
    let calendar_list_box;
    {
        let st = Rc::clone(&state);
        let mgc = Rc::clone(&month_grid_container_for_sidebar);
        let wgc = Rc::clone(&week_grid_container_for_sidebar);
        let dgc = Rc::clone(&day_grid_container_for_sidebar);
        let agc = Rc::clone(&agenda_container_for_sidebar);
        let cl = Rc::clone(client);
        let cl2 = Rc::clone(client);
        let sv = Rc::clone(&month_hooks_for_sidebar);
        let (sidebar_widget, list_box) =
            calendar_sidebar::build_calendar_sidebar(&state, client, move || {
                // Refresh all grids when visibility changes.
                if let (Some(container), Some(hooks)) = (&*mgc.borrow(), &*sv.borrow()) {
                    month_grid::refresh_month_grid(container, &st, &cl2, hooks);
                }
                if let Some(ref container) = *wgc.borrow() {
                    week_grid::refresh_week_grid(container, &st, &cl2);
                }
                if let Some(ref container) = *dgc.borrow() {
                    day_grid::refresh_day_grid(container, &st, &cl2);
                }
                if let Some(ref container) = *agc.borrow() {
                    event_list::refresh_agenda_view(container, &st, &cl);
                }
            });
        calendar_list_box = list_box;
        sidebar_box.append(&sidebar_widget);
    }

    // -----------------------------------------------------------------------
    // Main area
    // -----------------------------------------------------------------------
    let main_area = gtk::Box::new(gtk::Orientation::Vertical, 0);
    main_area.set_hexpand(true);

    // Page heading marker for unified E2E navigation tests.
    let heading_marker = gtk::Label::new(Some(events_strings::TITLE));
    heading_marker.set_height_request(0);
    heading_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&heading_marker, ids::PAGE_HEADING);
    main_area.append(&heading_marker);

    // --- Header bar ---
    let header_box = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    header_box.set_margin_top(8);
    header_box.set_margin_bottom(8);
    header_box.set_margin_start(12);
    header_box.set_margin_end(12);

    // Today button
    let today_btn = gtk::Button::with_label(common::TODAY);
    today_btn.add_css_class("flat");

    // Navigation arrows. These carry the canonical events-prev/next-month test
    // IDs (goal/ui/events.md: "Pan visible range") because they pan the MAIN
    // view's selected_date by the view-mode unit (step_backward/step_forward)
    // and update calendar-date-label. The mini-month's own prev/next browse the
    // sidebar picker only and are intentionally un-tagged.
    let prev_btn = gtk::Button::from_icon_name("go-previous-symbolic");
    prev_btn.add_css_class("flat");
    prev_btn.set_tooltip_text(Some(common::PREVIOUS));
    crate::testid::set_test_id(&prev_btn, ids::EVENTS_PREV_MONTH);

    let next_btn = gtk::Button::from_icon_name("go-next-symbolic");
    next_btn.add_css_class("flat");
    next_btn.set_tooltip_text(Some(common::NEXT));
    crate::testid::set_test_id(&next_btn, ids::EVENTS_NEXT_MONTH);

    // Date label (centered, hexpand)
    let date_label = gtk::Label::new(None);
    date_label.set_hexpand(true);
    date_label.set_halign(gtk::Align::Center);
    date_label.add_css_class("title-3");
    crate::testid::set_test_id(&date_label, ids::CALENDAR_DATE_LABEL);

    // Set initial label text
    update_date_label(&date_label, &state.borrow());

    // Build mini-month and insert at the top of the sidebar (before the
    // calendar sidebar widget that was appended earlier).
    {
        let st = Rc::clone(&state);
        let lbl = date_label.clone();
        let mgc = Rc::clone(&month_grid_container_for_sidebar);
        let wgc = Rc::clone(&week_grid_container_for_sidebar);
        let dgc = Rc::clone(&day_grid_container_for_sidebar);
        let agc = Rc::clone(&agenda_container_for_sidebar);
        let cl = Rc::clone(client);
        let cl2 = Rc::clone(client);
        let sv = Rc::clone(&month_hooks_for_sidebar);
        let mini = mini_month::build_mini_month(&state, move || {
            let s = st.borrow();
            update_date_label(&lbl, &s);
            drop(s);
            // Refresh all grids when a day is clicked in the mini-month.
            if let (Some(container), Some(hooks)) = (&*mgc.borrow(), &*sv.borrow()) {
                month_grid::refresh_month_grid(container, &st, &cl2, hooks);
            }
            if let Some(ref container) = *wgc.borrow() {
                week_grid::refresh_week_grid(container, &st, &cl2);
            }
            if let Some(ref container) = *dgc.borrow() {
                day_grid::refresh_day_grid(container, &st, &cl2);
            }
            if let Some(ref container) = *agc.borrow() {
                event_list::refresh_agenda_view(container, &st, &cl);
            }
        });
        sidebar_box.prepend(&mini);
    }

    // View mode toggle buttons (linked group)
    let mode_box = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    mode_box.add_css_class("linked");
    crate::testid::set_test_id(&mode_box, ids::EVENTS_VIEW_TOGGLE);

    let month_btn = gtk::ToggleButton::with_label(events_strings::VIEW_MONTH);
    let week_btn = gtk::ToggleButton::with_label(events_strings::VIEW_WEEK);
    let day_btn = gtk::ToggleButton::with_label(events_strings::VIEW_DAY);
    let agenda_btn = gtk::ToggleButton::with_label(events_strings::VIEW_AGENDA);
    crate::testid::set_test_id(&month_btn, ids::CALENDAR_VIEW_MONTH);
    crate::testid::set_test_id(&week_btn, ids::CALENDAR_VIEW_WEEK);
    crate::testid::set_test_id(&day_btn, ids::CALENDAR_VIEW_DAY);
    crate::testid::set_test_id(&agenda_btn, ids::CALENDAR_VIEW_AGENDA);

    // Group toggles so only one is active at a time
    week_btn.set_group(Some(&month_btn));
    day_btn.set_group(Some(&month_btn));
    agenda_btn.set_group(Some(&month_btn));

    // Initial state: activate button matching persisted view mode.
    match initial_view_mode {
        ViewMode::Month => month_btn.set_active(true),
        ViewMode::Week => week_btn.set_active(true),
        ViewMode::Day => day_btn.set_active(true),
        ViewMode::Agenda => agenda_btn.set_active(true),
    }

    mode_box.append(&month_btn);
    mode_box.append(&week_btn);
    mode_box.append(&day_btn);
    mode_box.append(&agenda_btn);

    // "New Event" button
    let new_event_btn = gtk::Button::with_label(events_strings::NEW_EVENT);
    new_event_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&new_event_btn, ids::NEW_EVENT_BTN);
    {
        let st = Rc::clone(&state);
        let cl = Rc::clone(client);
        new_event_btn.connect_clicked(move |btn| {
            // Author into the calendar the page is currently targeting — the
            // live selection, else the first visible / first listed one
            // (events.md § Layout & flow).
            let cal_id = st.borrow().authoring_target().unwrap_or_default();
            let today = time_utils::today();
            let data = super::event_form::PrefilledEventData {
                calendar_id: cal_id,
                summary: String::new(),
                start_time: format!("{:04}-{:02}-{:02} 09:00", today.0, today.1, today.2),
                end_time: format!("{:04}-{:02}-{:02} 10:00", today.0, today.1, today.2),
                description: String::new(),
                location: String::new(),
            };
            // This opener RESUMES the persisted draft rather than clearing it
            // (`events.md` § Persistence — clearing here is what would make the
            // `"events"` rail inert, since this is the opener a user reaches for
            // after relaunching to finish the event they were writing). The
            // day-cell gesture below is the one that starts fresh.
            let data = match super::drafts::resume_draft() {
                Some(draft) => data.with_draft(&draft),
                None => data,
            };
            let dialog = super::event_form::build_event_form_prefilled(&data, &cl);
            if let Some(root) = btn.root()
                && let Some(win) = root.downcast_ref::<gtk::Window>()
            {
                dialog.set_transient_for(Some(win));
            }
            dialog.present();
        });
    }

    // Assemble header
    header_box.append(&today_btn);
    header_box.append(&prev_btn);
    header_box.append(&next_btn);
    header_box.append(&date_label);
    header_box.append(&mode_box);
    header_box.append(&new_event_btn);

    main_area.append(&header_box);

    // Separator below header
    let sep = gtk::Separator::new(gtk::Orientation::Horizontal);
    main_area.append(&sep);

    // --- Main stack with placeholder pages ---
    let main_stack = gtk::Stack::new();
    main_stack.set_transition_type(gtk::StackTransitionType::Crossfade);
    main_stack.set_transition_duration(100);
    main_stack.set_vexpand(true);

    // The three non-month views are built first so `switch_view` (below) can
    // capture them: the month grid is handed that closure at construction, and
    // its drill-in target is the day grid.
    let week_grid_container = week_grid::build_week_grid(&state, client);
    let day_grid_container = day_grid::build_day_grid(&state, client);
    let agenda_container = event_list::build_agenda_view(&state, client);

    // The single programmatic view switch — see `ViewSwitch`. Sets the mode and
    // date, moves the stack, relabels the header, syncs the mode buttons, and
    // refreshes the view being switched to, so no caller can do half of it.
    let switch_view: ViewSwitch = {
        let st = Rc::clone(&state);
        let lbl = date_label.clone();
        let stk = main_stack.clone();
        let wgc = week_grid_container.clone();
        let dgc = day_grid_container.clone();
        let agc = agenda_container.clone();
        let cl = Rc::clone(client);
        let m_btn = month_btn.clone();
        let w_btn = week_btn.clone();
        let d_btn = day_btn.clone();
        let a_btn = agenda_btn.clone();
        let mgc_slot = Rc::clone(&month_grid_container_for_sidebar);
        // The month grid is built *from* this closure, so it can only be
        // reached back through the same late-bound slot the sidebar uses; both
        // slots are filled together, immediately below.
        let sv_slot = Rc::clone(&month_hooks_for_sidebar);
        // Re-entrancy guard: syncing the mode button below fires its own
        // `toggled` handler, which routes back here so that the button-driven
        // path and the programmatic one stay one implementation. The flag makes
        // the second entry a no-op instead of a second stack move + rebuild.
        let switching = Rc::new(Cell::new(false));
        Rc::new(move |mode: ViewMode, date: (i32, u32, u32)| {
            if switching.get() {
                return;
            }
            switching.set(true);
            {
                let mut s = st.borrow_mut();
                s.selected_date = date;
                s.view_mode = mode;
                update_date_label(&lbl, &s);
            }
            stk.set_visible_child_name(mode.as_wire());
            // Syncing the button fires its `toggled` handler, which routes
            // straight back here — the guard above absorbs that second entry.
            let btn = match mode {
                ViewMode::Month => &m_btn,
                ViewMode::Week => &w_btn,
                ViewMode::Day => &d_btn,
                ViewMode::Agenda => &a_btn,
            };
            if !btn.is_active() {
                btn.set_active(true);
            }
            match mode {
                ViewMode::Month => {
                    if let (Some(mgc), Some(hooks)) = (&*mgc_slot.borrow(), &*sv_slot.borrow()) {
                        month_grid::refresh_month_grid(mgc, &st, &cl, hooks);
                    }
                }
                ViewMode::Week => week_grid::refresh_week_grid(&wgc, &st, &cl),
                ViewMode::Day => day_grid::refresh_day_grid(&dgc, &st, &cl),
                ViewMode::Agenda => event_list::refresh_agenda_view(&agc, &st, &cl),
            }
            switching.set(false);
        })
    };

    // The quick-create compose is anchored to the header's "New Event" button:
    // it is always mapped, so the popover survives the drill-in's stack swap
    // (which unmaps the clicked cell), and it is a Button — the parent kind every
    // other `set_parent` in this app uses. Anchoring to the window instead leaves
    // the popover out of the widget tree entirely (measured 2026-07-30).
    let month_hooks = MonthGridHooks {
        switch_view: Rc::clone(&switch_view),
        presses: Rc::new(RefCell::new(day_cell_press::DayCellPress::new(
            day_cell_press::desktop_double_click_window(),
        ))),
        open_quick_create: {
            let st = Rc::clone(&state);
            let cl = Rc::clone(client);
            let anchor = new_event_btn.clone();
            Rc::new(move |date: (i32, u32, u32)| {
                // The cell names a DAY, not an instant, so the compose opens at
                // the shared working start — the same choice tui, windows and
                // apple make. Passing `None` here (as this did until
                // 2026-08-12) composes an all-day event instead, a divergence
                // the shared e2e could not see while it asserted only the
                // prefilled date. See `caltime::WORKING_DAY_START`.
                event_popover::show_quick_create(
                    &anchor,
                    date,
                    Some(fauna_core::caltime::WORKING_DAY_START),
                    &st,
                    &cl,
                );
            })
        },
    };
    *month_hooks_for_sidebar.borrow_mut() = Some(month_hooks.clone());

    let month_grid_container = month_grid::build_month_grid(&state, client, &month_hooks);

    main_stack.add_named(&month_grid_container, Some("month"));
    main_stack.add_named(&week_grid_container, Some("week"));
    main_stack.add_named(&day_grid_container, Some("day"));
    main_stack.add_named(&agenda_container, Some("agenda"));

    // Store the containers so the sidebar visibility callback can refresh them.
    *month_grid_container_for_sidebar.borrow_mut() = Some(month_grid_container.clone());
    *week_grid_container_for_sidebar.borrow_mut() = Some(week_grid_container.clone());
    *day_grid_container_for_sidebar.borrow_mut() = Some(day_grid_container.clone());
    *agenda_container_for_sidebar.borrow_mut() = Some(agenda_container.clone());

    main_stack.set_visible_child_name(initial_view_mode.as_wire());

    main_area.append(&main_stack);

    // -----------------------------------------------------------------------
    // Wire controls
    // -----------------------------------------------------------------------

    // Today / prev / next: the date moves, the view mode does not — so each is
    // the same `switch_view` call with a different date step.
    for (btn, step) in [
        (&today_btn, DateStep::Today),
        (&prev_btn, DateStep::Backward),
        (&next_btn, DateStep::Forward),
    ] {
        let st = Rc::clone(&state);
        let switch = Rc::clone(&switch_view);
        btn.connect_clicked(move |_| {
            let (mode, date) = {
                let mut s = st.borrow_mut();
                match step {
                    DateStep::Today => s.selected_date = time_utils::today(),
                    DateStep::Backward => step_backward(&mut s),
                    DateStep::Forward => step_forward(&mut s),
                }
                (s.view_mode, s.selected_date)
            };
            switch(mode, date);
        });
    }

    // View mode toggles. Each is a thin adapter onto `switch_view` — the same
    // implementation the drill-in uses, so the two paths cannot drift; the
    // button keeps the page's current date, only the mode changes.
    for (btn, mode) in [
        (&month_btn, ViewMode::Month),
        (&week_btn, ViewMode::Week),
        (&day_btn, ViewMode::Day),
        (&agenda_btn, ViewMode::Agenda),
    ] {
        let st = Rc::clone(&state);
        let switch = Rc::clone(&switch_view);
        btn.connect_toggled(move |btn| {
            if btn.is_active() {
                let date = st.borrow().selected_date;
                switch(mode, date);
            }
        });
    }

    // -----------------------------------------------------------------------
    // Keyboard navigation
    // -----------------------------------------------------------------------
    {
        let st = Rc::clone(&state);
        let switch = Rc::clone(&switch_view);

        let key_controller = gtk::EventControllerKey::new();
        key_controller.connect_key_pressed(move |_, key, _, modifier| {
            // Only handle bare key presses (no modifiers).
            if !modifier.is_empty() {
                return glib::Propagation::Proceed;
            }

            let action: Option<&str> = match key {
                gdk::Key::Left => Some("prev"),
                gdk::Key::Right => Some("next"),
                gdk::Key::Up => Some("prev"),
                gdk::Key::Down => Some("next"),
                gdk::Key::Page_Up => Some("prev_month"),
                gdk::Key::Page_Down => Some("next_month"),
                gdk::Key::Home => Some("today"),
                gdk::Key::t | gdk::Key::T => Some("today"),
                gdk::Key::m | gdk::Key::M => Some("mode_month"),
                gdk::Key::w | gdk::Key::W => Some("mode_week"),
                gdk::Key::d | gdk::Key::D => Some("mode_day"),
                gdk::Key::a | gdk::Key::A => Some("mode_agenda"),
                _ => None,
            };

            let action = match action {
                Some(a) => a,
                None => return glib::Propagation::Proceed,
            };

            // Compute the target (mode, date) only; `switch_view` owns every
            // effect of getting there.
            let (mode, date) = {
                let mut s = st.borrow_mut();
                match action {
                    "prev" => step_backward(&mut s),
                    "next" => step_forward(&mut s),
                    "prev_month" => {
                        let (y, m, d) = s.selected_date;
                        let (py, pm) = time_utils::prev_month(y, m);
                        s.selected_date = (py, pm, d.min(time_utils::days_in_month(py, pm)));
                    }
                    "next_month" => {
                        let (y, m, d) = s.selected_date;
                        let (ny, nm) = time_utils::next_month(y, m);
                        s.selected_date = (ny, nm, d.min(time_utils::days_in_month(ny, nm)));
                    }
                    "today" => {
                        s.selected_date = time_utils::today();
                    }
                    "mode_month" => {
                        s.view_mode = ViewMode::Month;
                    }
                    "mode_week" => {
                        s.view_mode = ViewMode::Week;
                    }
                    "mode_day" => {
                        s.view_mode = ViewMode::Day;
                    }
                    "mode_agenda" => {
                        s.view_mode = ViewMode::Agenda;
                    }
                    _ => {}
                }
                (s.view_mode, s.selected_date)
            };
            switch(mode, date);

            glib::Propagation::Stop
        });

        // The key controller needs to be on a focusable widget.
        // We'll add it to the main_area box.
        main_area.set_focusable(true);
        main_area.add_controller(key_controller);
    }

    // -----------------------------------------------------------------------
    // Outer container: sidebar + main area
    // -----------------------------------------------------------------------
    let outer = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    outer.append(&sidebar_box);

    // Vertical separator between sidebar and main area
    let vsep = gtk::Separator::new(gtk::Orientation::Vertical);
    outer.append(&vsep);

    outer.append(&main_area);

    // Persistent event-detail panel (right column) — the stable, reactive
    // surface that replaces the autohide popover (mirrors web/macOS split view).
    let detail_panel = event_detail::build_detail_panel(&state, client);
    let detail_sep = gtk::Separator::new(gtk::Orientation::Vertical);
    outer.append(&detail_sep);
    outer.append(&detail_panel);

    let handles = CalendarViewHandles {
        state,
        main_stack,
        date_label,
        sidebar_box: sidebar_box.clone(),
        calendar_list_box,
        month_grid_container,
        week_grid_container,
        day_grid_container,
        agenda_container,
        detail_panel,
        month_hooks,
    };

    (outer, handles)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rows::{CalendarRow, EventRow};

    fn cal(id: &str, name: &str) -> CalendarRow {
        CalendarRow {
            id: id.to_string(),
            name: name.to_string(),
            visibility: "private".to_string(),
        }
    }

    fn ev(id: &str, calendar_id: &str, start_time: &str) -> EventRow {
        EventRow {
            id: id.to_string(),
            calendar_id: calendar_id.to_string(),
            summary: format!("event {id}"),
            start_time: start_time.to_string(),
            end_time: None,
            location: None,
            description: None,
            attendance_mode: None,
            capacity: None,
        }
    }

    /// A bare state carrying `calendars` in the sidebar, all visible, nothing
    /// selected — the page's post-first-load resting shape.
    fn state_with(calendars: Vec<CalendarRow>) -> CalendarViewState {
        let visible = calendars.iter().map(|c| c.id.clone()).collect();
        CalendarViewState {
            selected_date: (2026, 7, 30),
            view_mode: ViewMode::Agenda,
            visible_calendars: visible,
            events_cache: Vec::new(),
            calendar_colors: HashMap::new(),
            attendees_cache: HashMap::new(),
            reminders_cache: HashMap::new(),
            selected_event: None,
            displayed_calendars: calendars,
            selected_calendar: None,
        }
    }

    // ── live_selection / shows_calendar ────────────────────────────────────

    /// No selection is the no-selection union: every listed calendar shows.
    #[test]
    fn no_selection_shows_every_calendar() {
        let s = state_with(vec![cal("aa", "Work"), cal("bb", "Home")]);
        assert_eq!(s.live_selection(), None);
        assert!(s.shows_calendar("aa"));
        assert!(s.shows_calendar("bb"));
    }

    /// A `calendar-item` click narrows the page to that calendar — the whole
    /// point of goal/ui/events.md § User actions' "Select calendar (filter
    /// visible events)", and what linux silently did NOT do until 2026-07-30.
    #[test]
    fn a_selection_narrows_to_that_calendar() {
        let mut s = state_with(vec![cal("aa", "Work"), cal("bb", "Home")]);
        s.selected_calendar = Some("bb".to_string());
        assert_eq!(s.live_selection(), Some("bb"));
        assert!(s.shows_calendar("bb"));
        assert!(!s.shows_calendar("aa"));
    }

    /// Selection beats the visibility checkboxes rather than intersecting with
    /// them: clicking a calendar whose checkbox is off still shows it, so the
    /// click can never land on a page that stays blank.
    #[test]
    fn a_selection_wins_over_an_unchecked_visibility_box() {
        let mut s = state_with(vec![cal("aa", "Work"), cal("bb", "Home")]);
        s.visible_calendars.remove("bb");
        s.selected_calendar = Some("bb".to_string());
        assert!(s.shows_calendar("bb"));
        assert!(!s.shows_calendar("aa"));
    }

    /// With nothing selected the checkboxes filter the union.
    #[test]
    fn visibility_filters_the_union_when_nothing_is_selected() {
        let mut s = state_with(vec![cal("aa", "Work"), cal("bb", "Home")]);
        s.visible_calendars.remove("bb");
        assert!(s.shows_calendar("aa"));
        assert!(!s.shows_calendar("bb"));
    }

    /// An empty visibility set is the pre-first-load state and means the union,
    /// not "hide everything" — the three grids used to read it the other way
    /// round from the agenda.
    #[test]
    fn an_empty_visibility_set_is_the_union() {
        let mut s = state_with(vec![cal("aa", "Work")]);
        s.visible_calendars.clear();
        assert!(s.shows_calendar("aa"));
    }

    /// A selection whose calendar has vanished reverts to the union rather than
    /// stranding the page on an empty list (the shared
    /// `fauna_client_caldav::resolve_calendar_selection` rule).
    #[test]
    fn a_stale_selection_reverts_to_the_union() {
        let mut s = state_with(vec![cal("aa", "Work")]);
        s.selected_calendar = Some("deleted-elsewhere".to_string());
        assert_eq!(s.live_selection(), None);
        assert!(s.shows_calendar("aa"));
    }

    // ── authoring_target ───────────────────────────────────────────────────

    /// New event / `.ics` import / `.ics` export target the selected calendar
    /// (events.md § Import / Export).
    #[test]
    fn authoring_targets_the_selection_first() {
        let mut s = state_with(vec![cal("aa", "Work"), cal("bb", "Home")]);
        s.selected_calendar = Some("bb".to_string());
        assert_eq!(s.authoring_target(), Some("bb".to_string()));
    }

    /// With no selection it is the first *visible* calendar in sidebar order —
    /// deterministic, unlike the `HashSet` iteration order this replaced.
    #[test]
    fn authoring_falls_back_to_the_first_visible_calendar_in_order() {
        let mut s = state_with(vec![cal("aa", "Work"), cal("bb", "Home")]);
        s.visible_calendars.remove("aa");
        assert_eq!(s.authoring_target(), Some("bb".to_string()));
    }

    /// Every box unchecked still authors somewhere — the first listed calendar.
    #[test]
    fn authoring_falls_back_to_the_first_listed_calendar() {
        let mut s = state_with(vec![cal("aa", "Work"), cal("bb", "Home")]);
        s.visible_calendars.clear();
        assert_eq!(s.authoring_target(), Some("aa".to_string()));
    }

    /// No calendars at all has no target (the caller must not invent one).
    #[test]
    fn authoring_has_no_target_without_calendars() {
        let s = state_with(Vec::new());
        assert_eq!(s.authoring_target(), None);
    }

    // ── index_events_by_date ───────────────────────────────────────────────

    /// The grids see exactly the scope the agenda does — one predicate, so a
    /// selection narrows the month/week/day grids too.
    #[test]
    fn the_grid_index_honours_the_selection() {
        let mut s = state_with(vec![cal("aa", "Work"), cal("bb", "Home")]);
        s.events_cache = vec![
            ev("1", "aa", "2026-07-30T10:00:00"),
            ev("2", "bb", "2026-07-30T14:00:00"),
        ];

        let unioned = index_events_by_date(&s);
        assert_eq!(unioned.get("2026-07-30").map(Vec::len), Some(2));

        s.selected_calendar = Some("bb".to_string());
        let narrowed = index_events_by_date(&s);
        let day = narrowed
            .get("2026-07-30")
            .expect("the day is still indexed");
        assert_eq!(day.len(), 1);
        assert_eq!(day[0].calendar_id, "bb");
    }
}
