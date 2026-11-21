use adw::prelude::*;
/// Month grid view: 6x7 grid of day cells with event bars.
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;
use std::time::Instant;

use super::calendar_view::{CalendarViewState, MonthGridHooks, ViewMode};
use super::day_cell_press;
use super::time_utils;
use crate::client::FaunaClient;
use crate::rows::EventRow;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the month grid container.
///
/// Returns a vertical `gtk::Box` containing the weekday header and the
/// 6x7 day-cell grid. Call `refresh_month_grid` to populate/update it.
///
/// `hooks` are the page callbacks the cells wire themselves to (see
/// [`MonthGridHooks`]). They are a parameter rather than page state on purpose:
/// a future caller that forgets them fails to compile, where a setter would
/// leave the cells silently inert.
pub fn build_month_grid(
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
    hooks: &MonthGridHooks,
) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_vexpand(true);
    outer.set_hexpand(true);
    // The "events-month-grid" test ID lives on a hidden marker label added by
    // refresh_month_grid, NOT on this Box: a plain gtk::Box has accessible role
    // Generic, which AT-SPI on Linux omits from the tree (so is_visible always
    // read false). Mirrors the week/day grid marker pattern.

    // Initial render.
    refresh_month_grid(&outer, state, client, hooks);

    outer
}

/// Rebuild the month grid to reflect the current state (selected date,
/// visible calendars, events cache, calendar colors).
pub fn refresh_month_grid(
    container: &gtk::Box,
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
    hooks: &MonthGridHooks,
) {
    // Clear everything.
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    // Hidden marker label so AT-SPI can find this container by test ID.
    // gtk::Box doesn't expose accessible Description to AT-SPI, but Label does.
    // Height >= 1px so AT-SPI reports SHOWING when this is the active Stack
    // child. Mirrors week_grid/day_grid.
    let marker = gtk::Label::new(None);
    marker.set_height_request(1);
    marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&marker, ids::EVENTS_MONTH_GRID);
    container.append(&marker);

    let s = state.borrow();
    let (year, month, _day) = s.selected_date;
    let week_start = time_utils::locale_week_start();

    // -----------------------------------------------------------------------
    // Weekday header row
    // -----------------------------------------------------------------------
    let header_grid = gtk::Grid::new();
    header_grid.set_column_homogeneous(true);
    header_grid.set_margin_start(4);
    header_grid.set_margin_end(4);

    for col in 0u32..7 {
        let dow = (week_start + col) % 7;
        let lbl = gtk::Label::new(Some(time_utils::weekday_short(dow)));
        lbl.add_css_class("dim-label");
        lbl.add_css_class("caption");
        lbl.set_halign(gtk::Align::Center);
        lbl.set_margin_top(4);
        lbl.set_margin_bottom(4);
        header_grid.attach(&lbl, col as i32, 0, 1, 1);
    }
    container.append(&header_grid);

    // -----------------------------------------------------------------------
    // Build events-by-date index (only visible calendars)
    // -----------------------------------------------------------------------
    let events_by_date = super::calendar_view::index_events_by_date(&s);

    // -----------------------------------------------------------------------
    // Day grid: 6 rows x 7 columns
    // -----------------------------------------------------------------------
    let grid = gtk::Grid::new();
    grid.set_column_homogeneous(true);
    grid.set_row_homogeneous(true);
    grid.set_vexpand(true);
    grid.set_hexpand(true);
    grid.set_margin_start(4);
    grid.set_margin_end(4);

    let today = time_utils::today();
    let cells = time_utils::month_grid(year, month, week_start);

    let on_event_bar: Rc<dyn Fn(&EventRow)> = {
        let cl = Rc::clone(client);
        Rc::new(move |ev: &EventRow| {
            cl.select_event(Some(ev.clone()));
            cl.fetch_attendees(&ev.calendar_id, &ev.id);
        })
    };

    for (idx, &(cy, cm, cd)) in cells.iter().enumerate() {
        let col = (idx % 7) as i32;
        let row = (idx / 7) as i32;

        let cell = build_day_cell(
            cy,
            cm,
            cd,
            year,
            month,
            today,
            &events_by_date,
            &s.calendar_colors,
            &on_event_bar,
            hooks,
        );
        grid.attach(&cell, col, row, 1, 1);
    }

    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&grid)
        .build();
    container.append(&scroll);
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a single day cell: day number + up to 3 event bars + "+N more".
///
/// The cell is a flat `gtk::Button` wrapping the content box — the shape
/// windows uses (`EventsPage.xaml.cs`), and the one that makes the Outlook
/// single-click drill-in reachable: a bare `gtk::Box` carrying only a
/// `GestureClick` has no activation for the keyboard, for AT-SPI, or for the
/// e2e agent to invoke. Event bars inside stay their own `gtk::Button`s, so a
/// click on one is claimed by that bar and never reaches this cell —
/// `events.md` § Layout & flow's "a click on an `event-card` is not a day-cell
/// click".
#[allow(clippy::too_many_arguments)]
fn build_day_cell(
    cy: i32,
    cm: u32,
    cd: u32,
    display_year: i32,
    display_month: u32,
    today: (i32, u32, u32),
    events_by_date: &HashMap<String, Vec<EventRow>>,
    calendar_colors: &HashMap<String, String>,
    on_event_bar: &Rc<dyn Fn(&EventRow)>,
    hooks: &MonthGridHooks,
) -> gtk::Button {
    let cell = gtk::Box::new(gtk::Orientation::Vertical, 1);
    cell.set_overflow(gtk::Overflow::Hidden);

    let cell_button = gtk::Button::new();
    cell_button.set_child(Some(&cell));
    cell_button.add_css_class("flat");
    cell_button.add_css_class("day-cell");

    // `events-day-cell-{YYYY-MM-DD}` (indexed) — ui.yaml's `month-grid-view`
    // component. Stamped BEFORE the tooltip below: `set_test_id` writes the
    // tooltip too, so the reverse order replaces the real accessibility text
    // with the literal id.
    crate::testid::set_test_id(
        &cell_button,
        &format!("events-day-cell-{:04}-{:02}-{:02}", cy, cm, cd),
    );

    // Dimmed if out of display month.
    if cy != display_year || cm != display_month {
        cell_button.add_css_class("day-cell-dimmed");
    }

    // Accessibility tooltip: "Monday March 16, 3 events"
    {
        let date_key_for_tt = format!("{:04}-{:02}-{:02}", cy, cm, cd);
        let event_count = events_by_date
            .get(&date_key_for_tt)
            .map(|v| v.len())
            .unwrap_or(0);
        let dow = time_utils::day_of_week(cy, cm, cd);
        let tt = if event_count > 0 {
            format!(
                "{} {} {}, {}",
                time_utils::weekday_name(dow),
                time_utils::month_name(cm),
                cd,
                crate::i18n::event_count(event_count as i64)
            )
        } else {
            format!(
                "{} {} {}",
                time_utils::weekday_name(dow),
                time_utils::month_name(cm),
                cd
            )
        };
        cell_button.set_tooltip_text(Some(&tt));
    }

    // Day number label (right-aligned).
    let day_label = gtk::Label::new(Some(&cd.to_string()));
    day_label.set_halign(gtk::Align::End);
    day_label.set_margin_top(2);
    day_label.set_margin_end(4);
    day_label.add_css_class("caption");

    if (cy, cm, cd) == today {
        day_label.add_css_class("today-marker");
    }

    cell.append(&day_label);

    // Event bars.
    let date_key = format!("{:04}-{:02}-{:02}", cy, cm, cd);
    let max_bars = 3;

    if let Some(day_events) = events_by_date.get(&date_key) {
        let show_count = day_events.len().min(max_bars);
        for ev in &day_events[..show_count] {
            let color = calendar_colors
                .get(&ev.calendar_id)
                .map(|s| s.as_str())
                .unwrap_or("slate");

            // Use a Button so AT-SPI do_action(0) triggers the clicked signal.
            let bar = gtk::Button::with_label(&ev.summary);
            bar.set_halign(gtk::Align::Fill);
            bar.add_css_class("event-bar");
            bar.add_css_class("flat");
            bar.add_css_class(&format!("cal-{}-bg", color));
            // E2E test IDs so tests can count and read event summaries.
            crate::testid::set_test_id(&bar, ids::EVENT_CARD);

            // Separate hidden label carrying "event-card-summary" for
            // get_text() calls. Height >= 1px so AT-SPI reports SHOWING.
            let summary_marker = gtk::Label::new(Some(&ev.summary));
            summary_marker.set_height_request(1);
            summary_marker.set_overflow(gtk::Overflow::Hidden);
            crate::testid::set_test_id(&summary_marker, ids::EVENT_CARD_SUMMARY);

            // Accessibility tooltip.
            {
                let tt = if let (Some((sh, sm)), Some(et)) =
                    (time_utils::parse_time(&ev.start_time), ev.end_time.as_ref())
                {
                    if let Some((eh, em)) = time_utils::parse_time(et) {
                        let sa = if sh < 12 { "AM" } else { "PM" };
                        let ea = if eh < 12 { "AM" } else { "PM" };
                        let sh12 = if sh == 0 {
                            12
                        } else if sh > 12 {
                            sh - 12
                        } else {
                            sh
                        };
                        let eh12 = if eh == 0 {
                            12
                        } else if eh > 12 {
                            eh - 12
                        } else {
                            eh
                        };
                        format!(
                            "{}, {}:{:02} {} - {}:{:02} {}",
                            ev.summary, sh12, sm, sa, eh12, em, ea
                        )
                    } else {
                        ev.summary.clone()
                    }
                } else {
                    ev.summary.clone()
                };
                bar.set_tooltip_text(Some(&tt));
            }

            // Click on event bar: select it in the persistent detail panel and
            // fetch its attendees (the panel re-renders reactively). Behind a
            // callback rather than the client itself, so a tier_1 test can build
            // a real cell without one — the same seam, and the same reason, as
            // `week_grid::add_time_slot_markers`.
            {
                let ev_clone = ev.clone();
                let on_bar = Rc::clone(on_event_bar);
                bar.connect_clicked(move |_| on_bar(&ev_clone));
            }

            cell.append(&bar);
            cell.append(&summary_marker);
        }

        // "+N more" if overflow.
        if day_events.len() > max_bars {
            let remaining = day_events.len() - max_bars;
            let more_label = gtk::Label::new(Some(&format!("+{} more", remaining)));
            more_label.add_css_class("dim-label");
            more_label.add_css_class("caption");
            more_label.set_halign(gtk::Align::Start);
            more_label.set_margin_start(4);

            // No click handler of its own: the label is a plain child of the
            // cell button, so clicking it drills through the cell like any other
            // part of the day. It used to carry a gesture that set `view_mode`
            // and logged — which nothing observed, leaving the page claiming Day
            // view while the stack still showed Month.
            cell.append(&more_label);
        }
    }

    // Single-click a day → Day view for that date; double-click → the
    // quick-create compose prefilled with it (events.md § Layout & flow, the
    // Outlook model). Both gestures live on this one widget, so which of them a
    // press turns out to be is decided by `day_cell_press` — see that module
    // for why acting on the first click immediately made the second one
    // unreachable for a real user.
    {
        let hooks = hooks.clone();
        cell_button.connect_clicked(move |_| {
            let outcome = hooks
                .presses
                .borrow_mut()
                .click((cy, cm, cd), Instant::now());
            apply_press(&hooks, outcome);
        });
    }

    // GTK's own double-press detection is the pair's second half. It arrives
    // *before* that press's release, so the `clicked` above still owes us one
    // more event — `DayCellPress::double_press` swallows it.
    //
    // Two non-obvious choices, both load-bearing:
    //
    // - The gesture is in the capture phase, so the button's own bubble-phase
    //   gesture cannot claim the sequence out from under it.
    // - The popover is anchored to the header's New Event button (see
    //   `calendar_view.rs`), not to this cell and not to the window. Before the
    //   deferral the drill had already unmapped the cell by this point, and a
    //   popover popped up against an unmapped anchor silently never becomes
    //   visible — `gtk::Popover::popup()` leaves `is_visible()` false, so the
    //   automation walk's `is_showing` prune drops it and the compose reads as
    //   `count=0`. That, not any gap in the walk, is what made the e2e
    //   unobservable under the earlier window/page-Box anchors; the walk itself
    //   descends into popovers fine (pinned by
    //   `automation::find::tests::descends_into_a_popped_up_popover`). The
    //   anchor stays put now that the cell survives the pair, because a cell
    //   the user is about to compose *from* is not a stable anchor either: the
    //   compose's own submit repaints the grid.
    //
    // A transient `event_form` dialog would also survive the unmap, and is what
    // `new-event-btn` opens — but it *stays open*, and it carries the same
    // `event-summary` / `event-dtstart` / `create-event` ids as the real
    // compose. A left-open dialog therefore captures the next test's
    // `create_event`, which then submits into the wrong calendar. Measured, not
    // theorised: it turned `test_event_create_and_delete[linux]` red while that
    // test passes in isolation. An autohide popover dismisses on the next click
    // and leaves nothing behind.
    let gesture = gtk::GestureClick::new();
    gesture.set_propagation_phase(gtk::PropagationPhase::Capture);
    {
        let hooks = hooks.clone();
        gesture.connect_pressed(move |_, n_press, _, _| {
            if n_press == 2 {
                let outcome = hooks
                    .presses
                    .borrow_mut()
                    .double_press((cy, cm, cd), Instant::now());
                apply_press(&hooks, outcome);
            }
        });
    }
    cell_button.add_controller(gesture);

    cell_button
}

/// Carry out what a press decided, and arm the timer a held one needs.
///
/// The borrow on the press chain is released before anything here runs:
/// `switch_view` rebuilds every cell in this grid, and those cells' handlers
/// borrow the same `RefCell`.
fn apply_press(hooks: &MonthGridHooks, outcome: day_cell_press::Outcome) {
    if let Some(date) = outcome.drill {
        (hooks.switch_view)(ViewMode::Day, date);
    }
    if let Some(date) = outcome.compose {
        (hooks.open_quick_create)(date);
    }
    if let Some(due) = outcome.hold_until {
        arm_expiry(hooks, due);
    }
}

/// Wake up when a held press's window closes and drill in, since the user has
/// simply stopped interacting and no further input is coming.
///
/// Not a wall-clock *assertion*: `expire` re-checks the deadline itself, so an
/// early or duplicate wake-up is harmless (`e2e-conventions.md` point 14). It
/// has to be, because glib rounds a timeout down to whole milliseconds and will
/// fire a hair BEFORE the deadline — measured, and it stranded the held press
/// permanently until this re-armed on the chain's own deadline instead of
/// trusting one shot.
fn arm_expiry(hooks: &MonthGridHooks, due: Instant) {
    let hooks = hooks.clone();
    glib::timeout_add_local_once(due.saturating_duration_since(Instant::now()), move || {
        let drill = hooks.presses.borrow_mut().expire(Instant::now());
        if let Some(date) = drill {
            (hooks.switch_view)(ViewMode::Day, date);
            return;
        }
        // Either the window had not quite closed, or a second press already
        // resolved the chain. `deadline` tells which, and re-arming on it can
        // only ever converge: the chain's deadline moves solely on a new press.
        let pending = hooks.presses.borrow().deadline();
        if let Some(due) = pending {
            arm_expiry(&hooks, due);
        }
    });
}

/// The **human** pointer path at a month day cell: the door the automation
/// agent structurally bypasses.
///
/// The agent's `actuate_double_click` fires `n_press: 2` at the *retained
/// widget reference* (`automation::agent::press_gesture`), so it is immune to
/// whether a real pointer's second press would still resolve to that widget —
/// correct for convention 14, and precisely why a green e2e proved nothing
/// about a hand here. This suite asks GTK itself, through real layout and real
/// hit-testing, what a second press would land on.
///
/// Before the deferral (`day_cell_press`) the answer was: not the cell. The
/// drill-in swapped the `gtk::Stack` synchronously inside `clicked`, unmapping
/// the cell, and the point the user pressed at belonged to an
/// `events-time-slot-*` of the day timeline that had just repainted underneath
/// — so the compose opened at *the slot's* time, or not at all. Same class of
/// bug as windows' and tui's (`docs/goal/ui/events.md` § Implementation status
/// today, the *day cell's two gestures collide* bullet).
#[cfg(test)]
mod tests {
    use super::*;
    use crate::views::events::week_grid;
    use std::cell::Cell;
    use std::time::Duration;

    /// A press chain short enough that a test never waits long for the timer,
    /// while still being a real deferral window.
    const TEST_WINDOW: Duration = Duration::from_millis(30);
    const CELL: (i32, u32, u32) = (2026, 7, 9);

    /// Pump to a *deadline* on observable state, never a fixed amount of work
    /// (`e2e-conventions.md` point 14): a green run stops the instant the
    /// condition holds and pays nothing for the ceiling, while a loaded machine
    /// gets as long as it needs.
    const PUMP_BUDGET: Duration = Duration::from_secs(30);

    fn pump_until(cond: impl Fn() -> bool) -> bool {
        let ctx = gtk::glib::MainContext::default();
        let deadline = Instant::now() + PUMP_BUDGET;
        loop {
            if cond() {
                return true;
            }
            if Instant::now() >= deadline {
                return false;
            }
            if !ctx.iteration(false) {
                std::thread::sleep(Duration::from_millis(1));
            }
        }
    }

    /// What the page did, as the cell's own hooks report it.
    #[derive(Default)]
    struct Page {
        drilled: RefCell<Vec<(i32, u32, u32)>>,
        composed: RefCell<Vec<(i32, u32, u32)>>,
        /// What the page does on top of recording a compose — a real popover,
        /// for the test that needs the popup's pointer grab.
        on_compose: RefCell<Option<Box<dyn Fn()>>>,
    }

    /// The real production cell, in a real stack whose other page is the real
    /// day timeline — the geometry that makes the fall-through available.
    #[allow(clippy::type_complexity)]
    fn month_page() -> (gtk::Window, gtk::Button, gtk::Stack, Rc<Page>) {
        month_page_with(TEST_WINDOW)
    }

    #[allow(clippy::type_complexity)]
    fn month_page_with(window: Duration) -> (gtk::Window, gtk::Button, gtk::Stack, Rc<Page>) {
        let _ = adw::init();
        let page = Rc::new(Page::default());
        let stack = gtk::Stack::new();

        // The day page the drill-in lands on: the same 96 quarter-hour
        // `events-time-slot-{HH-MM}` markers production tiles it with, so a
        // press that falls through lands on one exactly as it would for a user.
        let day = gtk::Fixed::new();
        day.set_hexpand(true);
        day.set_vexpand(true);
        week_grid::add_time_slot_markers(&day, |_: &gtk::Widget, _, _| {});
        stack.add_named(&day, Some("day"));

        let month_container = gtk::Box::new(gtk::Orientation::Vertical, 0);
        stack.add_named(&month_container, Some("month"));

        let hooks = {
            let stack = stack.clone();
            let drill_page = Rc::clone(&page);
            let composed_page = Rc::clone(&page);
            MonthGridHooks {
                switch_view: Rc::new(move |_mode, date| {
                    drill_page.drilled.borrow_mut().push(date);
                    // What production's `switch_view` does that matters here:
                    // move the stack, which unmaps every widget on the month
                    // page (`calendar_view.rs`).
                    stack.set_visible_child_name("day");
                }),
                open_quick_create: Rc::new(move |date| {
                    composed_page.composed.borrow_mut().push(date);
                    if let Some(open) = composed_page.on_compose.borrow().as_ref() {
                        open();
                    }
                }),
                presses: Rc::new(RefCell::new(day_cell_press::DayCellPress::new(window))),
            }
        };

        let on_event_bar: Rc<dyn Fn(&EventRow)> = Rc::new(|_| {});
        let cell = build_day_cell(
            CELL.0,
            CELL.1,
            CELL.2,
            CELL.0,
            CELL.1,
            (2026, 7, 1),
            &HashMap::new(),
            &HashMap::new(),
            &on_event_bar,
            &hooks,
        );
        cell.set_size_request(120, 80);
        cell.set_hexpand(true);
        cell.set_vexpand(true);
        month_container.append(&cell);

        stack.set_visible_child_name("month");

        let window = gtk::Window::new();
        // A window group of its own: GTK routes pointer events to the grab
        // widget of the window's group, and every ungrouped window shares the
        // default one — so a modal window another test on the shared GTK
        // thread left up took this cell's real presses, the pair never reached
        // `n_press == 2`, and the double-click test was red in the full suite
        // only.
        gtk::WindowGroup::new().add_window(&window);
        window.set_default_size(400, 300);
        window.set_child(Some(&stack));
        window.present();
        assert!(
            pump_until(|| window.is_mapped() && cell.is_mapped()),
            "the test window never mapped within {PUMP_BUDGET:?}"
        );

        (window, cell, stack, page)
    }

    /// Which widget a pointer press at the cell's centre would actually reach —
    /// GTK's own hit-test over the real allocation, not our lookup by id.
    fn picked_at_cell_centre(window: &gtk::Window, cell: &gtk::Button) -> Option<gtk::Widget> {
        let bounds = cell
            .compute_bounds(window)
            .expect("the cell has an allocation");
        let centre = bounds.center();
        window.pick(
            centre.x() as f64,
            centre.y() as f64,
            gtk::PickFlags::DEFAULT,
        )
    }

    /// Whether `widget` is the cell or something inside it — a press landing on
    /// the day-number label is still a press on the cell.
    fn is_within(widget: Option<gtk::Widget>, cell: &gtk::Button) -> bool {
        let mut node = widget;
        while let Some(w) = node {
            if w.eq(cell.upcast_ref::<gtk::Widget>()) {
                return true;
            }
            node = w.parent();
        }
        false
    }

    /// **The regression this track was opened for.** After the first click, the
    /// point the user pressed must still belong to the cell — otherwise their
    /// second press cannot possibly reach the gesture that opens the compose.
    ///
    /// Before the deferral this failed exactly as predicted: the cell was
    /// unmapped and the pick returned an `events-time-slot-*` of the day
    /// timeline underneath.
    #[test]
    fn the_cell_still_owns_its_own_coordinates_after_the_first_click() {
        crate::testid::run_on_gtk_thread(|| {
            let (window, cell, stack, page) = month_page();

            assert!(
                is_within(picked_at_cell_centre(&window, &cell), &cell),
                "a press at the cell's centre must reach the cell to begin with"
            );

            cell.emit_clicked();

            let picked = picked_at_cell_centre(&window, &cell);
            let picked_id = picked.as_ref().map(|w| w.widget_name().to_string());
            assert!(
                is_within(picked, &cell),
                "the second half of a double click must still land on the cell; \
                 it landed on {picked_id:?} instead, and the drill-in had already \
                 run ({:?}) — the exact shape of the bug",
                page.drilled.borrow()
            );
            assert!(cell.is_mapped(), "the cell must not be unmapped yet");
            assert_eq!(
                stack.visible_child_name().map(|s| s.to_string()).as_deref(),
                Some("month"),
                "the drill-in must wait for the double-click window to close"
            );
            assert!(page.drilled.borrow().is_empty(), "nothing drilled yet");

            window.destroy();
        });
    }

    /// The pair closes: the compose opens prefilled with the pressed date, and
    /// the drill-in never runs — not even once the window has long closed.
    #[test]
    fn a_real_pair_composes_that_date_and_never_drills() {
        crate::testid::run_on_gtk_thread(|| {
            let (window, cell, stack, page) = month_page();

            // The two halves of a double click, in GTK's own order: the first
            // press's release, then the second press reported as `n_press: 2`.
            cell.emit_clicked();
            let gesture = cell
                .observe_controllers()
                .into_iter()
                .flatten()
                .find_map(|c| c.downcast::<gtk::GestureClick>().ok())
                .expect("the cell carries its capture-phase click gesture");
            gesture.emit_by_name::<()>("pressed", &[&2i32, &0.0f64, &0.0f64]);
            // ...and the release GTK still owes us for that same press.
            cell.emit_clicked();

            assert!(
                pump_until(|| !page.composed.borrow().is_empty()),
                "the compose never opened"
            );
            assert_eq!(*page.composed.borrow(), vec![CELL]);

            // Negative assert anchored to a causal barrier, never a settle
            // sleep: a timeout armed for well past the press window must have
            // fired before we can conclude no drill-in is pending.
            let barrier = Rc::new(Cell::new(false));
            {
                let barrier = Rc::clone(&barrier);
                gtk::glib::timeout_add_local_once(TEST_WINDOW * 4, move || barrier.set(true));
            }
            assert!(pump_until(|| barrier.get()), "the barrier never fired");

            assert!(
                page.drilled.borrow().is_empty(),
                "the held click must never drill after its pair closed: {:?}",
                page.drilled.borrow()
            );
            assert_eq!(
                stack.visible_child_name().map(|s| s.to_string()).as_deref(),
                Some("month")
            );

            window.destroy();
        });
    }

    /// A lone click still drills in — the deferral delays the drill-in, it does
    /// not remove it.
    #[test]
    fn a_lone_click_still_drills_in_once_the_window_closes() {
        crate::testid::run_on_gtk_thread(|| {
            let (window, cell, stack, page) = month_page();

            cell.emit_clicked();
            assert!(
                pump_until(|| !page.drilled.borrow().is_empty()),
                "a single click never drilled in within {PUMP_BUDGET:?}"
            );
            assert_eq!(*page.drilled.borrow(), vec![CELL]);
            assert_eq!(
                stack.visible_child_name().map(|s| s.to_string()).as_deref(),
                Some("day")
            );
            assert!(
                page.composed.borrow().is_empty(),
                "no compose was asked for"
            );

            window.destroy();
        });
    }

    /// A real pointer on the private Xvfb, through the XTEST extension.
    ///
    /// The tests above emit the gestures' signals directly, which is exactly
    /// the door that cannot see what a popover's pointer grab does to the cell
    /// underneath: GTK's crossing bookkeeping runs only for events the display
    /// server delivers. libX11 and libXtst are the system's own (they ship with
    /// the X stack the private display already needs), opened at run time so
    /// the crate gains no dependency.
    type XDisplay = *mut std::ffi::c_void;
    type XOpenDisplayFn = unsafe extern "C" fn(*const libc::c_char) -> XDisplay;
    type XTestFakeMotionEventFn = unsafe extern "C" fn(XDisplay, i32, i32, i32, u64) -> i32;
    type XTestFakeButtonEventFn = unsafe extern "C" fn(XDisplay, u32, i32, u64) -> i32;
    type XSyncFn = unsafe extern "C" fn(XDisplay, i32) -> i32;

    struct XPointer {
        dpy: XDisplay,
        motion: XTestFakeMotionEventFn,
        button: XTestFakeButtonEventFn,
        sync: XSyncFn,
    }

    impl XPointer {
        /// Connect to `display` — the GTK test thread's private Xvfb.
        fn open(display: &str) -> Self {
            // SAFETY: plain C entry points of the system's libX11/libXtst,
            // looked up by name and called with the signatures their headers
            // declare; each pointer is checked before it is used.
            unsafe {
                let x11 = libc::dlopen(c"libX11.so.6".as_ptr(), libc::RTLD_NOW);
                let xtst = libc::dlopen(c"libXtst.so.6".as_ptr(), libc::RTLD_NOW);
                assert!(
                    !x11.is_null() && !xtst.is_null(),
                    "libX11.so.6 / libXtst.so.6 not loadable — the real-pointer \
                     tests need the X stack the private display runs on"
                );
                let sym = |lib: *mut std::ffi::c_void, name: &std::ffi::CStr| {
                    let f = libc::dlsym(lib, name.as_ptr());
                    assert!(!f.is_null(), "missing X symbol {name:?}");
                    f
                };
                let open = std::mem::transmute::<*mut std::ffi::c_void, XOpenDisplayFn>(sym(
                    x11,
                    c"XOpenDisplay",
                ));
                let name = std::ffi::CString::new(display).expect("display name");
                let dpy = open(name.as_ptr());
                assert!(!dpy.is_null(), "XOpenDisplay({display}) failed");
                XPointer {
                    dpy,
                    motion: std::mem::transmute::<*mut std::ffi::c_void, XTestFakeMotionEventFn>(
                        sym(xtst, c"XTestFakeMotionEvent"),
                    ),
                    button: std::mem::transmute::<*mut std::ffi::c_void, XTestFakeButtonEventFn>(
                        sym(xtst, c"XTestFakeButtonEvent"),
                    ),
                    sync: std::mem::transmute::<*mut std::ffi::c_void, XSyncFn>(sym(x11, c"XSync")),
                }
            }
        }

        /// Move the pointer to root coordinates `(x, y)`.
        fn move_to(&self, x: i32, y: i32) {
            // SAFETY: `dpy` is the live connection `open` checked.
            unsafe {
                (self.motion)(self.dpy, -1, x, y, 0);
                (self.sync)(self.dpy, 0);
            }
        }

        /// Press (`down`) or release the primary button.
        fn button(&self, down: bool) {
            // SAFETY: as `move_to`.
            unsafe {
                (self.button)(self.dpy, 1, i32::from(down), 0);
                (self.sync)(self.dpy, 0);
            }
        }
    }

    /// **No tint is left on the day after the compose opens from it**
    /// (`docs/goal/ui/events.md` § *No calendar to hold an event*, ruling 4: a
    /// failed save is reported in the compose and nowhere else — a day that
    /// looks touched is a report of an event that does not exist).
    ///
    /// The tests above emit the gestures' signals straight at the widgets, so
    /// they cannot see what the compose's pointer grab does to GTK's crossing
    /// bookkeeping. This one drives a real double click through the display
    /// server, opens a real autohide popover the way `show_quick_create` does —
    /// anchored to a separate header button, as production anchors it to
    /// `new-event-btn` — moves the pointer away as a person reaching for the
    /// compose does, and reads the cell's state flags once each step has
    /// settled.
    ///
    /// What it measured: the cell's `:hover` (`PRELIGHT`) and `:active` are both
    /// clear. A popover's grab DOES strand `:hover` — on the popover's *parent*
    /// and its ancestors, not on the widget that was pressed — so this cell
    /// stays clean exactly as long as the compose is not parented to it (with
    /// the cell as the parent, the cell stayed hovered with the pointer gone,
    /// 2 of 2 runs). Hence the assertion on the anchor too: it is the witness
    /// that the grab really was taken, so a green cell is not a vacuous one.
    #[test]
    fn a_real_double_click_leaves_no_hover_or_press_on_the_day() {
        crate::testid::run_on_gtk_thread(|| {
            // The test owns its double-click window: GTK reports `n_press == 2`
            // only inside `gtk-double-click-time`, and a loaded box can delay
            // the second press past the desktop's value, so both the setting
            // and the press chain are pinned far above any load delay.
            let _ = adw::init();
            let pinned = Duration::from_secs(10);
            gtk::Settings::default()
                .expect("the GTK test thread has settings")
                .set_gtk_double_click_time(pinned.as_millis() as i32);
            let (window, cell, stack, page) = month_page_with(pinned);
            let month = stack
                .child_by_name("month")
                .and_then(|w| w.downcast::<gtk::Box>().ok())
                .expect("the month page is a box");
            let anchor = gtk::Button::with_label("New Event");
            anchor.set_halign(gtk::Align::Start);
            month.prepend(&anchor);

            let popover = gtk::Popover::new();
            popover.set_parent(&anchor);
            popover.set_autohide(true);
            popover.set_child(Some(&gtk::Entry::new()));
            {
                let popover = popover.clone();
                *page.on_compose.borrow_mut() = Some(Box::new(move || popover.popup()));
            }

            let display = gtk::gdk::Display::default()
                .expect("the GTK test thread has its display")
                .name()
                .to_string();
            let pointer = XPointer::open(&display);
            assert!(
                pump_until(|| window.width() > 0 && cell.width() > 0 && anchor.width() > 0),
                "the test window was never allocated"
            );
            let bounds = cell.compute_bounds(&window).expect("the cell is allocated");
            // The undecorated test window sits at the root origin.
            let (cx, cy) = (
                (bounds.x() + bounds.width() / 2.0) as i32,
                (bounds.y() + bounds.height() / 2.0) as i32,
            );
            let has = |w: &gtk::Button, f: gtk::StateFlags| w.state_flags().contains(f);
            let hover = gtk::StateFlags::PRELIGHT;
            let press = gtk::StateFlags::ACTIVE;

            // A crossing can clear a flag and set it again in the same burst
            // (measured: `:hover` read clear 0.3 ms after the leave and set
            // again by 200 ms), so every reading is taken after a causal
            // barrier, never at the first instant a condition holds and never
            // after a wall-clock wait. The barrier is two observed facts: the
            // crossing the reading depends on reached the cell's own motion
            // controller (counted below), and the main context has then run
            // every source already ready — each X request above ends in an
            // `XSync`, so the server's events are on GTK's connection by now.
            let crossings = Rc::new(Cell::new((0u32, 0u32)));
            let watch = gtk::EventControllerMotion::new();
            {
                let c = Rc::clone(&crossings);
                watch.connect_enter(move |_, _, _| {
                    let (e, l) = c.get();
                    c.set((e + 1, l));
                });
                let c = Rc::clone(&crossings);
                watch.connect_leave(move |_| {
                    let (e, l) = c.get();
                    c.set((e, l + 1));
                });
            }
            cell.add_controller(watch);
            let drain = || {
                let ctx = gtk::glib::MainContext::default();
                let mut spins = 0;
                while ctx.pending() && spins < 10_000 {
                    ctx.iteration(false);
                    spins += 1;
                }
            };

            pointer.move_to(cx, cy);
            assert!(
                pump_until(|| has(&cell, hover) && crossings.get().0 >= 1),
                "reading 1 (hover after the pointer entered the cell): the pointer \
                 never reached the cell — the harness, not the product"
            );

            // A real double click, in the display server's own order.
            pointer.button(true);
            assert!(
                pump_until(|| has(&cell, press)),
                "reading 2 (press after the first button-down): the cell never \
                 went :active, state {:?}",
                cell.state_flags()
            );
            pointer.button(false);
            assert!(
                pump_until(|| !has(&cell, press)),
                "reading 3 (press after the first button-up): the cell stayed \
                 :active, state {:?}",
                cell.state_flags()
            );
            pointer.button(true);
            assert!(
                pump_until(|| !page.composed.borrow().is_empty() && popover.is_visible()),
                "a real double click never opened the compose"
            );
            pointer.button(false);
            drain();

            // The person reaches for the compose (here: off the window, so no
            // ordinary crossing can put the hover back).
            let leaves_before = crossings.get().1;
            pointer.move_to(1000, 900);
            assert!(
                pump_until(|| crossings.get().1 > leaves_before && has(&anchor, hover)),
                "reading 4 (the compose's grab): the cell saw no leave, or the \
                 anchor never kept the stranded hover — the grab was not taken, so \
                 the cell reading below would prove nothing; cell {:?}, anchor {:?}",
                cell.state_flags(),
                anchor.state_flags()
            );
            drain();
            assert!(
                !has(&cell, hover) && !has(&cell, press),
                "reading 5 (cell with the compose open and the pointer gone): the \
                 day reads {:?} — tinted as if an event had been added",
                cell.state_flags()
            );

            // The compose closes.
            popover.popdown();
            assert!(
                pump_until(|| !popover.is_visible()),
                "reading 6: the compose never closed"
            );
            drain();
            assert!(
                !has(&cell, hover) && !has(&cell, press),
                "reading 7 (cell after the compose closed): the day reads {:?}",
                cell.state_flags()
            );
            assert!(page.drilled.borrow().is_empty(), "the pair never drills");

            popover.unparent();
            window.destroy();
        });
    }

    fn ev(id: &str) -> EventRow {
        EventRow {
            id: id.to_string(),
            calendar_id: "cal".to_string(),
            summary: format!("event {id}"),
            start_time: "09:00".to_string(),
            end_time: None,
            location: None,
            description: None,
            attendance_mode: None,
            capacity: None,
        }
    }

    /// The day-cell accessibility tooltip's event-count clause goes through
    /// the shared `fauna_core::format::event_count` i18n decision — never a
    /// hand-rolled "1 event"/"N events" pair
    /// (`docs/goal/behavior/value-formatting.md` § Event count). Zero events
    /// renders no count clause at all.
    #[test]
    fn the_tooltip_localizes_the_event_count() {
        crate::testid::run_on_gtk_thread(|| {
            let _ = adw::init();
            let on_event_bar: Rc<dyn Fn(&EventRow)> = Rc::new(|_| {});
            let hooks = MonthGridHooks {
                switch_view: Rc::new(|_, _| {}),
                open_quick_create: Rc::new(|_| {}),
                presses: Rc::new(RefCell::new(day_cell_press::DayCellPress::new(TEST_WINDOW))),
            };
            let date_key = format!("{:04}-{:02}-{:02}", CELL.0, CELL.1, CELL.2);
            let no_events = HashMap::new();
            let mut one_event = HashMap::new();
            one_event.insert(date_key.clone(), vec![ev("a")]);
            let mut many_events = HashMap::new();
            many_events.insert(date_key, vec![ev("a"), ev("b"), ev("c")]);

            for (events_by_date, expected_suffix) in [
                (&no_events, None),
                (&one_event, Some(", 1 event")),
                (&many_events, Some(", 3 events")),
            ] {
                let cell = build_day_cell(
                    CELL.0,
                    CELL.1,
                    CELL.2,
                    CELL.0,
                    CELL.1,
                    (2000, 1, 1),
                    events_by_date,
                    &HashMap::new(),
                    &on_event_bar,
                    &hooks,
                );
                let tt = cell.tooltip_text().expect("tooltip is always set");
                match expected_suffix {
                    Some(suffix) => assert!(
                        tt.ends_with(suffix),
                        "tooltip {tt:?} must end with {suffix:?}"
                    ),
                    None => assert!(
                        !tt.contains("event"),
                        "zero events must carry no count clause, got {tt:?}"
                    ),
                }
            }
        });
    }
}
