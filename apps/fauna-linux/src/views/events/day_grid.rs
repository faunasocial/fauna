use adw::prelude::*;
/// Day grid view: single-day time grid with positioned event blocks.
/// Shares the column rendering logic with `week_grid`.
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::calendar_view::CalendarViewState;
use super::time_utils;
use super::time_utils::HALF_HOUR_PX;
use super::week_grid;
use crate::client::FaunaClient;
use crate::rows::EventRow;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the day grid container.
pub fn build_day_grid(
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_vexpand(true);
    outer.set_hexpand(true);

    refresh_day_grid(&outer, state, client);

    outer
}

/// Rebuild the day grid to reflect the current state.
pub fn refresh_day_grid(
    container: &gtk::Box,
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) {
    // Clear everything.
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    // Hidden marker label so AT-SPI can find this container by test ID.
    // gtk::Box doesn't expose accessible Description to AT-SPI, but Label does.
    // Height must be >= 1px for AT-SPI to report SHOWING state; 0px widgets
    // are treated as not-showing even when their parent is the active Stack child.
    let marker = gtk::Label::new(None);
    marker.set_height_request(1);
    marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&marker, ids::CALENDAR_DAY_TIMELINE);
    container.append(&marker);

    let s = state.borrow();
    let (year, month, day) = s.selected_date;
    let today = time_utils::today();
    let is_today = (year, month, day) == today;

    // Index events by date (only visible calendars).
    let events_by_date = super::calendar_view::index_events_by_date(&s);
    let date_key = format!("{:04}-{:02}-{:02}", year, month, day);

    // -----------------------------------------------------------------------
    // All-day banner
    // -----------------------------------------------------------------------
    let all_day_events = collect_all_day_events(&date_key, &events_by_date, &s.calendar_colors);

    if !all_day_events.is_empty() {
        let banner = gtk::Box::new(gtk::Orientation::Horizontal, 4);
        banner.add_css_class("all-day-banner");
        banner.set_margin_start(56);
        // ui.yaml `calendar-allday-band`, painted only while the day has an
        // all-day event (as on tui). `set_test_id` stamps the id as the tooltip;
        // a band-wide tooltip would be noise, so clear it (the slot markers'
        // idiom).
        crate::testid::set_test_id(&banner, ids::CALENDAR_ALLDAY_BAND);
        banner.set_tooltip_text(None);

        let label = gtk::Label::new(Some(crate::i18n::strings::events::ALL_DAY));
        label.add_css_class("dim-label");
        label.add_css_class("caption");
        label.set_margin_end(8);
        banner.append(&label);

        for (ev, color) in &all_day_events {
            let chip = gtk::Label::new(Some(&ev.summary));
            chip.set_ellipsize(gtk::pango::EllipsizeMode::End);
            chip.set_max_width_chars(30);
            chip.add_css_class("all-day-chip");
            chip.add_css_class(&format!("cal-{}-bg", color));
            banner.append(&chip);
        }

        container.append(&banner);
    }

    // -----------------------------------------------------------------------
    // Scrollable time grid
    // -----------------------------------------------------------------------
    let scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .build();

    let time_grid = gtk::Box::new(gtk::Orientation::Horizontal, 0);

    // Hour labels column (shared from week_grid).
    let hour_col = week_grid::build_hour_labels();
    time_grid.append(&hour_col);

    // Single day column (full width).
    let day_events = week_grid::get_timed_events(&date_key, &events_by_date, &s.calendar_colors);
    let fixed = week_grid::build_day_column_fixed(&day_events, (year, month, day), state, client);
    fixed.set_hexpand(true);
    fixed.add_css_class("day-column");

    // Current time line (only if today).
    if is_today {
        week_grid::add_current_time_line(&fixed, 400);
    }

    time_grid.append(&fixed);

    scroll.set_child(Some(&time_grid));

    // Scroll to ~8am on first build.
    let adj = scroll.vadjustment();
    glib::idle_add_local_once(move || {
        let target = 8.0 * 2.0 * HALF_HOUR_PX;
        adj.set_value(target);
    });

    container.append(&scroll);
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Collect all-day events for a single date.
fn collect_all_day_events(
    date_key: &str,
    events_by_date: &HashMap<String, Vec<EventRow>>,
    calendar_colors: &HashMap<String, String>,
) -> Vec<(EventRow, String)> {
    let mut result = Vec::new();

    if let Some(day_events) = events_by_date.get(date_key) {
        for ev in day_events {
            let end_ref = ev.end_time.as_deref();
            if time_utils::is_all_day(&ev.start_time, end_ref) {
                let color = calendar_colors
                    .get(&ev.calendar_id)
                    .cloned()
                    .unwrap_or_else(|| "slate".to_string());
                result.push((ev.clone(), color));
            }
        }
    }

    result
}
