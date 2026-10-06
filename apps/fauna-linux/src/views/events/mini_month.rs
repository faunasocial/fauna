/// Mini-month navigator widget for the calendar sidebar.
///
/// Displays a compact month grid with prev/next navigation. Clicking a day
/// updates `CalendarViewState::selected_date` and fires the `on_date_changed`
/// callback.
use adw::prelude::*;
use std::cell::RefCell;
use std::rc::Rc;

use super::calendar_view::CalendarViewState;
use super::time_utils;

// ---------------------------------------------------------------------------
// Internal display state (separate from the main CalendarViewState so the
// mini-month can be scrolled independently of the selected date).
// ---------------------------------------------------------------------------

#[derive(Clone, Copy)]
struct DisplayMonth {
    year: i32,
    month: u32,
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the mini month widget.
///
/// `on_date_changed` is called whenever the user clicks a day cell.
pub fn build_mini_month(
    state: &Rc<RefCell<CalendarViewState>>,
    on_date_changed: impl Fn() + 'static,
) -> gtk::Box {
    let (sel_year, sel_month, _) = state.borrow().selected_date;

    // The mini-month has its own display month, initialised to the selected date.
    let display = Rc::new(RefCell::new(DisplayMonth {
        year: sel_year,
        month: sel_month,
    }));

    // Wrap the callback so it can be shared across multiple button closures.
    let on_date_changed = Rc::new(on_date_changed);

    // -----------------------------------------------------------------------
    // Outer container
    // -----------------------------------------------------------------------
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 4);
    outer.set_margin_top(4);
    outer.set_margin_bottom(4);

    // -----------------------------------------------------------------------
    // Navigation row: [<]  "March 2026"  [>]
    // -----------------------------------------------------------------------
    let nav_row = gtk::Box::new(gtk::Orientation::Horizontal, 0);
    nav_row.set_halign(gtk::Align::Fill);

    // The mini-month nav browses the sidebar picker's display month only; the
    // canonical events-prev/next-month test IDs live on the MAIN nav arrows
    // (calendar_view.rs) which pan the visible range. Leaving these un-tagged
    // avoids a duplicate ID.
    let prev_btn = gtk::Button::from_icon_name("go-previous-symbolic");
    prev_btn.add_css_class("flat");
    prev_btn.add_css_class("mini-month-nav");

    let next_btn = gtk::Button::from_icon_name("go-next-symbolic");
    next_btn.add_css_class("flat");
    next_btn.add_css_class("mini-month-nav");

    let month_label = gtk::Label::new(None);
    month_label.set_hexpand(true);
    month_label.set_halign(gtk::Align::Center);
    month_label.add_css_class("heading");

    nav_row.append(&prev_btn);
    nav_row.append(&month_label);
    nav_row.append(&next_btn);
    outer.append(&nav_row);

    // -----------------------------------------------------------------------
    // Day-of-week header row
    // -----------------------------------------------------------------------
    let week_start = time_utils::locale_week_start();
    let header_grid = gtk::Grid::new();
    header_grid.set_column_homogeneous(true);

    for col in 0u32..7 {
        // Map column index back to the weekday number (0=Mon..6=Sun).
        let dow = (week_start + col).rem_euclid(7);
        // Use only the first letter of the short weekday name for compactness.
        let short = time_utils::weekday_short(dow);
        let first_char: String = short.chars().next().unwrap_or('?').to_string();
        let lbl = gtk::Label::new(Some(&first_char));
        lbl.add_css_class("dim-label");
        lbl.add_css_class("mini-month-header");
        lbl.set_halign(gtk::Align::Center);
        header_grid.attach(&lbl, col as i32, 0, 1, 1);
    }
    outer.append(&header_grid);

    // -----------------------------------------------------------------------
    // Day grid (gtk::Grid, 7 cols × 6 rows)
    // -----------------------------------------------------------------------
    let day_grid = gtk::Grid::new();
    day_grid.set_column_homogeneous(true);
    day_grid.set_row_homogeneous(true);
    outer.append(&day_grid);

    // -----------------------------------------------------------------------
    // Initial population
    // -----------------------------------------------------------------------
    populate_grid(
        &day_grid,
        &month_label,
        &display,
        state,
        &on_date_changed,
        week_start,
    );

    // -----------------------------------------------------------------------
    // Wire prev/next navigation
    // -----------------------------------------------------------------------
    {
        let display = Rc::clone(&display);
        let state = Rc::clone(state);
        let day_grid = day_grid.clone();
        let month_label = month_label.clone();
        let on_date_changed = Rc::clone(&on_date_changed);
        prev_btn.connect_clicked(move |_| {
            {
                let mut d = display.borrow_mut();
                let (py, pm) = if d.month == 1 {
                    (d.year - 1, 12)
                } else {
                    (d.year, d.month - 1)
                };
                d.year = py;
                d.month = pm;
            }
            populate_grid(
                &day_grid,
                &month_label,
                &display,
                &state,
                &on_date_changed,
                week_start,
            );
        });
    }
    {
        let display = Rc::clone(&display);
        let state = Rc::clone(state);
        let day_grid = day_grid.clone();
        let month_label = month_label.clone();
        let on_date_changed = Rc::clone(&on_date_changed);
        next_btn.connect_clicked(move |_| {
            {
                let mut d = display.borrow_mut();
                let (ny, nm) = if d.month == 12 {
                    (d.year + 1, 1)
                } else {
                    (d.year, d.month + 1)
                };
                d.year = ny;
                d.month = nm;
            }
            populate_grid(
                &day_grid,
                &month_label,
                &display,
                &state,
                &on_date_changed,
                week_start,
            );
        });
    }

    outer
}

/// Rebuild the mini month grid to reflect current state.
///
/// Call this after an external date change (e.g. the "Today" button in the
/// main header) to keep the mini-month in sync.
#[allow(dead_code)]
pub fn refresh_mini_month(container: &gtk::Box, state: &Rc<RefCell<CalendarViewState>>) {
    // The day grid is the third child (index 2): nav_row(0), header_grid(1), day_grid(2).
    // We locate it by iterating children and finding the gtk::Grid.
    let mut child = container.first_child();
    let mut grid_widget: Option<gtk::Grid> = None;
    let mut label_widget: Option<gtk::Label> = None;

    while let Some(w) = child {
        if let Ok(g) = w.clone().downcast::<gtk::Grid>() {
            // The day grid is the second Grid (first is the header row).
            // We can distinguish by checking it is row-homogeneous (the day grid is).
            if g.is_row_homogeneous() {
                grid_widget = Some(g);
            }
        }
        if let Ok(nav) = w.clone().downcast::<gtk::Box>() {
            // The nav_row is the first child Box; find the Label inside it.
            let mut nav_child = nav.first_child();
            while let Some(nc) = nav_child {
                if let Ok(lbl) = nc.clone().downcast::<gtk::Label>()
                    && lbl.has_css_class("heading")
                {
                    label_widget = Some(lbl);
                }
                nav_child = nc.next_sibling();
            }
        }
        child = w.next_sibling();
    }

    if let (Some(grid), Some(label)) = (grid_widget, label_widget) {
        let (sel_year, sel_month, _) = state.borrow().selected_date;
        // Sync display month to the newly selected date.
        let display = Rc::new(RefCell::new(DisplayMonth {
            year: sel_year,
            month: sel_month,
        }));
        let week_start = time_utils::locale_week_start();
        // Use a no-op callback — external caller handles post-refresh logic.
        let noop = Rc::new(|| {});
        populate_grid(&grid, &label, &display, state, &noop, week_start);
    }
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Clear and repopulate the day grid and month label.
fn populate_grid(
    grid: &gtk::Grid,
    month_label: &gtk::Label,
    display: &Rc<RefCell<DisplayMonth>>,
    state: &Rc<RefCell<CalendarViewState>>,
    on_date_changed: &Rc<impl Fn() + 'static>,
    week_start: u32,
) {
    // Update the month/year label.
    let (disp_year, disp_month) = {
        let d = display.borrow();
        (d.year, d.month)
    };
    month_label.set_label(&format!(
        "{} {}",
        time_utils::month_name(disp_month),
        disp_year
    ));

    // Remove all existing buttons from the grid.
    while let Some(child) = grid.first_child() {
        grid.remove(&child);
    }

    let today = time_utils::today();
    let selected = state.borrow().selected_date;

    let cells = time_utils::month_grid(disp_year, disp_month, week_start);

    for (idx, &(cy, cm, cd)) in cells.iter().enumerate() {
        let col = (idx % 7) as i32;
        let row = (idx / 7) as i32;

        let btn = gtk::Button::with_label(&cd.to_string());
        btn.add_css_class("flat");
        btn.add_css_class("mini-month-day");

        // Dimmed if outside the display month.
        if cy != disp_year || cm != disp_month {
            btn.add_css_class("day-cell-dimmed");
        }

        // Today marker.
        if (cy, cm, cd) == today {
            btn.add_css_class("today-marker");
        }

        // Selected marker.
        if (cy, cm, cd) == selected {
            btn.add_css_class("mini-month-selected");
        }

        // Accessibility tooltip: full date string.
        {
            let dow = time_utils::day_of_week(cy, cm, cd);
            let tt = format!(
                "{}, {} {}, {}",
                time_utils::weekday_name(dow),
                time_utils::month_name(cm),
                cd,
                cy,
            );
            btn.set_tooltip_text(Some(&tt));
        }

        // Wire click → update state and call the callback.
        {
            let state = Rc::clone(state);
            let display = Rc::clone(display);
            let grid = grid.clone();
            let month_label = month_label.clone();
            let on_date_changed = Rc::clone(on_date_changed);
            btn.connect_clicked(move |_| {
                // Update the selected date in the shared state.
                state.borrow_mut().selected_date = (cy, cm, cd);
                // Sync the mini-month display month to the clicked date so
                // that selection stays visible even if it was a leading/trailing
                // day from an adjacent month.
                {
                    let mut d = display.borrow_mut();
                    d.year = cy;
                    d.month = cm;
                }
                // Repopulate to refresh CSS classes.
                populate_grid(
                    &grid,
                    &month_label,
                    &display,
                    &state,
                    &on_date_changed,
                    time_utils::locale_week_start(),
                );
                on_date_changed();
            });
        }

        grid.attach(&btn, col, row, 1, 1);
    }
}
