use adw::prelude::*;
/// Week grid view: 7-day time grid with positioned event blocks.
use fauna_ui_ids as ids;
use gtk::gdk;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::calendar_view::CalendarViewState;
use super::event_popover;
use super::time_utils;
use super::time_utils::HALF_HOUR_PX;
use crate::client::FaunaClient;
use crate::rows::EventRow;

/// Height per quarter-hour quick-create slot marker.
const QUARTER_HOUR_PX: f64 = HALF_HOUR_PX / 2.0;
/// Total column height: 48 half-hours * 30px = 1440px.
const COLUMN_HEIGHT: f64 = 48.0 * HALF_HOUR_PX;
/// Minimum event block height.
const MIN_BLOCK_HEIGHT: f64 = HALF_HOUR_PX;

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the week grid container.
pub fn build_week_grid(
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_vexpand(true);
    outer.set_hexpand(true);

    refresh_week_grid(&outer, state, client);

    outer
}

/// Rebuild the week grid to reflect the current state.
pub fn refresh_week_grid(
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
    crate::testid::set_test_id(&marker, ids::CALENDAR_WEEK_GRID);
    container.append(&marker);

    let s = state.borrow();
    let (year, month, day) = s.selected_date;
    let ws = time_utils::locale_week_start();
    let (wy, wm, wd) = time_utils::week_start_date(year, month, day, ws);

    // Build the 7 dates for this week.
    let week_dates: Vec<(i32, u32, u32)> = (0..7)
        .map(|i| time_utils::add_days(wy, wm, wd, i))
        .collect();

    let today = time_utils::today();

    // Index events by date (only visible calendars).
    let events_by_date = super::calendar_view::index_events_by_date(&s);

    // -----------------------------------------------------------------------
    // Day header row
    // -----------------------------------------------------------------------
    let header_grid = gtk::Grid::new();
    header_grid.set_column_homogeneous(false);

    // Spacer for hour-label column.
    let spacer = gtk::Label::new(None);
    spacer.set_width_request(56);
    header_grid.attach(&spacer, 0, 0, 1, 1);

    for (col, &(dy, dm, dd)) in week_dates.iter().enumerate() {
        let dow = time_utils::day_of_week(dy, dm, dd);
        let text = format!("{} {}", time_utils::weekday_short(dow), dd);
        let lbl = gtk::Label::new(Some(&text));
        lbl.set_hexpand(true);
        lbl.set_halign(gtk::Align::Center);
        lbl.add_css_class("week-day-header");
        if (dy, dm, dd) == today {
            lbl.add_css_class("week-day-header-today");
            lbl.add_css_class("accent");
        }
        header_grid.attach(&lbl, (col + 1) as i32, 0, 1, 1);
    }

    container.append(&header_grid);

    // -----------------------------------------------------------------------
    // All-day banner
    // -----------------------------------------------------------------------
    let all_day_events = collect_all_day_events(&week_dates, &events_by_date, &s.calendar_colors);
    if !all_day_events.is_empty() {
        let banner = gtk::Box::new(gtk::Orientation::Horizontal, 0);
        banner.add_css_class("all-day-banner");
        // ui.yaml `calendar-allday-band` — see `day_grid.rs` for the tooltip.
        crate::testid::set_test_id(&banner, ids::CALENDAR_ALLDAY_BAND);
        banner.set_tooltip_text(None);

        // Spacer for hour-label column.
        let sp = gtk::Label::new(Some(crate::i18n::strings::events::ALL_DAY));
        sp.set_width_request(56);
        sp.add_css_class("dim-label");
        sp.add_css_class("caption");
        sp.set_halign(gtk::Align::End);
        sp.set_margin_end(4);
        banner.append(&sp);

        for col in 0..7 {
            let col_box = gtk::Box::new(gtk::Orientation::Vertical, 1);
            col_box.set_hexpand(true);

            if let Some(evts) = all_day_events.get(&col) {
                for (ev, color) in evts {
                    let chip = gtk::Label::new(Some(&ev.summary));
                    chip.set_ellipsize(gtk::pango::EllipsizeMode::End);
                    chip.set_max_width_chars(14);
                    chip.add_css_class("all-day-chip");
                    chip.add_css_class(&format!("cal-{}-bg", color));
                    col_box.append(&chip);
                }
            }

            banner.append(&col_box);
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

    // Hour labels column.
    let hour_col = build_hour_labels();
    time_grid.append(&hour_col);

    // 7 day columns.
    let today_col_index = week_dates.iter().position(|d| *d == today);

    for (col, &(dy, dm, dd)) in week_dates.iter().enumerate() {
        let date_key = format!("{:04}-{:02}-{:02}", dy, dm, dd);
        let day_events = get_timed_events(&date_key, &events_by_date, &s.calendar_colors);

        let fixed = build_day_column_fixed(&day_events, (dy, dm, dd), state, client);
        fixed.set_hexpand(true);
        fixed.add_css_class("day-column");

        // Current time line (only for today).
        if today_col_index == Some(col) {
            add_current_time_line(&fixed, 200);
        }

        time_grid.append(&fixed);
    }

    scroll.set_child(Some(&time_grid));

    // Scroll to ~8am on first build.
    let adj = scroll.vadjustment();
    glib::idle_add_local_once(move || {
        let target = 8.0 * 2.0 * HALF_HOUR_PX; // 8:00 AM
        adj.set_value(target);
    });

    container.append(&scroll);
}

// ---------------------------------------------------------------------------
// Shared: build a single day column as gtk::Fixed with positioned event blocks
// ---------------------------------------------------------------------------

/// Build a `gtk::Fixed` day column with positioned event blocks.
/// Used by both week_grid (per-day) and day_grid (single column).
///
/// `events`: Vec of (EventRow, color_name, start_minutes, end_minutes).
/// `date`: the date this column represents.
pub fn build_day_column_fixed(
    events: &[(EventRow, String, u32, u32)],
    date: (i32, u32, u32),
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) -> gtk::Fixed {
    let fixed = gtk::Fixed::new();
    fixed.set_size_request(-1, COLUMN_HEIGHT as i32);

    // Empty-slot quick-create: one 15-minute marker per slot, tiling the whole
    // 24h column UNDER the event blocks (ui.yaml `events-time-slot-{HH-MM}`).
    // Blocks are added after the markers, so GTK picking prefers them and a
    // click on a block bubbles block → fixed, never into a slot sibling — the
    // slots need no hit-test of their own, and the column itself carries no
    // background gesture any more (the pre-2026-08-01 background gesture also
    // fired for clicks landing ON a block: two bubble-phase gestures, neither
    // claiming). Each slot's time is closure-captured, never derived from
    // pointer y — which is also what makes slots drivable headlessly: the
    // agent's gesture emission carries (0,0), so a y-computed time would
    // always have read 00:00.
    let slot_widgets = {
        let st = Rc::clone(state);
        let cl = Rc::clone(client);
        add_time_slot_markers(&fixed, move |anchor, hour, min| {
            event_popover::show_quick_create(anchor, date, Some((hour, min)), &st, &cl);
        })
    };

    // Build time intervals for overlap detection.
    let intervals: Vec<(u32, u32)> = events.iter().map(|(_, _, s, e)| (*s, *e)).collect();
    let overlaps = time_utils::find_overlaps(&intervals);

    // We need to know the allocated width at render time, but for Fixed positioning
    // we use a relative approach: set each block's width via size_request and
    // reposition on size-allocate. For simplicity, use a well-known column width
    // estimate and rely on hexpand.
    //
    // Strategy: place blocks using a "notify::width" signal to re-layout when
    // the column width changes.
    let events_rc: Rc<Vec<(EventRow, String, u32, u32)>> = Rc::new(events.to_vec());
    let overlaps_rc: Rc<Vec<(usize, u32, u32)>> = Rc::new(overlaps);

    // Use a size-allocate approach: on each allocation, reposition blocks.
    // First, create all the block widgets and store them.
    let block_widgets: Rc<RefCell<Vec<gtk::Box>>> = Rc::new(RefCell::new(Vec::new()));

    {
        let mut blocks = block_widgets.borrow_mut();
        for (ev, color, start_min, end_min) in events_rc.iter() {
            let duration = if end_min > start_min {
                end_min - start_min
            } else {
                30 // minimum 30 min
            };
            let block_height = ((duration as f64) / 30.0 * HALF_HOUR_PX).max(MIN_BLOCK_HEIGHT);
            let y_pos = (*start_min as f64) / 30.0 * HALF_HOUR_PX;

            let block = gtk::Box::new(gtk::Orientation::Vertical, 0);
            block.add_css_class("event-block");
            block.add_css_class(&format!("cal-{}-bg", color));
            block.set_size_request(-1, block_height as i32);
            block.set_overflow(gtk::Overflow::Hidden);
            // E2E test ID so tests can count and read event summaries (via the
            // descendant-label join fallback in `automation::find::text_of`).
            // Must come before the tooltip is set below — `set_test_id` also
            // stamps the tooltip, and the real per-event tooltip must win.
            crate::testid::set_test_id(&block, ids::CALENDAR_EVENT_BLOCK);

            // Time label.
            let time_text = format!("{:02}:{:02}", start_min / 60, start_min % 60);
            let time_lbl = gtk::Label::new(Some(&time_text));
            time_lbl.add_css_class("caption");
            time_lbl.set_halign(gtk::Align::Start);
            block.append(&time_lbl);

            // Summary label.
            let summary_lbl = gtk::Label::new(Some(&ev.summary));
            summary_lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
            summary_lbl.set_halign(gtk::Align::Start);
            summary_lbl.set_max_width_chars(20);
            block.append(&summary_lbl);

            // Accessibility tooltip: "Team standup, 9:00 AM - 9:30 AM"
            let start_h = start_min / 60;
            let start_m = start_min % 60;
            let end_h = end_min / 60;
            let end_m = end_min % 60;
            let start_ampm = if start_h < 12 { "AM" } else { "PM" };
            let end_ampm = if end_h < 12 { "AM" } else { "PM" };
            let start_h12 = if start_h == 0 {
                12
            } else if start_h > 12 {
                start_h - 12
            } else {
                start_h
            };
            let end_h12 = if end_h == 0 {
                12
            } else if end_h > 12 {
                end_h - 12
            } else {
                end_h
            };
            let tooltip = format!(
                "{}, {}:{:02} {} - {}:{:02} {}",
                ev.summary, start_h12, start_m, start_ampm, end_h12, end_m, end_ampm
            );
            block.set_tooltip_text(Some(&tooltip));

            // Drag source: drag-to-move event blocks.
            let drag_source = gtk::DragSource::new();
            drag_source.set_actions(gdk::DragAction::MOVE);
            let event_id = ev.id.clone();
            drag_source.connect_prepare(move |_, _, _| {
                Some(gdk::ContentProvider::for_value(&event_id.to_value()))
            });
            drag_source.connect_drag_begin(|source, _drag| {
                if let Some(widget) = source.widget() {
                    let icon = gtk::WidgetPaintable::new(Some(&widget));
                    source.set_icon(Some(&icon), 0, 0);
                }
            });
            block.add_controller(drag_source);

            // Click handler: select the event in the persistent detail panel and
            // fetch its attendees (the panel re-renders reactively).
            let gesture = gtk::GestureClick::new();
            let ev_clone = ev.clone();
            let cl = Rc::clone(client);
            gesture.connect_released(move |_, _, _, _| {
                cl.select_event(Some(ev_clone.clone()));
                cl.fetch_attendees(&ev_clone.calendar_id, &ev_clone.id);
            });
            block.add_controller(gesture);

            fixed.put(&block, 0.0, y_pos);
            blocks.push(block);
        }
    }

    // Drop target on the day column: accept dragged event IDs.
    {
        let day_date = date;
        let drop_target = gtk::DropTarget::new(glib::Type::STRING, gdk::DragAction::MOVE);
        drop_target.connect_drop(move |_target, value, _x, y| {
            if let Ok(event_id) = value.get::<String>() {
                let hour = (y as u32) / 60;
                let minute = ((y as u32) % 60) / 30 * 30;
                tracing::debug!(
                    "drag-move event {} to {:?} {:02}:{:02}",
                    event_id,
                    day_date,
                    hour,
                    minute
                );
                true
            } else {
                false
            }
        });
        fixed.add_controller(drop_target);
    }

    // On resize, reposition blocks according to overlap columns and stretch
    // the slot markers to the column width (gtk::Fixed does no sizing of its
    // own, so an unstretched marker would be a 0-width pick target for a real
    // pointer — the agent's headless gesture emission doesn't care, but a
    // user's click does).
    {
        let bw = Rc::clone(&block_widgets);
        let ev = Rc::clone(&events_rc);
        let ov = Rc::clone(&overlaps_rc);
        let slots = slot_widgets;
        let f = fixed.clone();

        // Use the "notify" on allocation to resize.
        // gtk::Fixed doesn't have a direct size-allocate signal in gtk4-rs 0.9,
        // so we watch the width property change via a tick callback that runs once
        // after layout.
        let did_layout = Rc::new(RefCell::new(0i32));
        let dl = Rc::clone(&did_layout);
        f.add_tick_callback(move |widget, _clock| {
            let w = widget.width();
            let mut prev = dl.borrow_mut();
            if w > 0 && w != *prev {
                *prev = w;
                let col_width = w as f64;
                for marker in slots.iter() {
                    marker.set_size_request(w, QUARTER_HOUR_PX as i32);
                }
                let blocks = bw.borrow();
                for (idx, block) in blocks.iter().enumerate() {
                    let (_ei, col_idx, total_cols) = ov[idx];
                    let (_, _, start_min, _end_min) = &ev[idx];
                    let y_pos = (*start_min as f64) / 30.0 * HALF_HOUR_PX;
                    let bw_val = col_width / (total_cols.max(1) as f64);
                    let x_pos = (col_idx as f64) * bw_val;

                    block.set_size_request(bw_val as i32 - 2, block.height_request());
                    let fixed_w = widget.downcast_ref::<gtk::Fixed>().unwrap();
                    fixed_w.move_(block, x_pos, y_pos);
                }
            }
            glib::ControlFlow::Continue
        });
    }

    fixed
}

/// Tile one 15-minute quick-create slot marker per slot across the 24h column,
/// each carrying the indexed ui.yaml id `events-time-slot-{HH-MM}` and its own
/// `GestureClick` that hands `on_slot` the slot's snapped time. The callback
/// seam (rather than taking state + client directly) is what makes the
/// renderer unit-testable without a `FaunaClient`.
pub(super) fn add_time_slot_markers(
    fixed: &gtk::Fixed,
    on_slot: impl Fn(&gtk::Widget, u32, u32) + Clone + 'static,
) -> Vec<gtk::Box> {
    let mut slots = Vec::with_capacity(24 * 4);
    for slot in 0..(24u32 * 4) {
        let minutes = slot * 15;
        let (hour, min) = (minutes / 60, minutes % 60);
        let marker = gtk::Box::new(gtk::Orientation::Vertical, 0);
        crate::testid::set_test_id(&marker, &format!("events-time-slot-{hour:02}-{min:02}"));
        // `set_test_id` stamps the id as the tooltip; on slots that would pop
        // a tooltip over every pixel of empty grid space, so clear it — the
        // same "the real tooltip must win" ordering the event block uses, the
        // real tooltip here being none.
        marker.set_tooltip_text(None);
        marker.set_size_request(-1, QUARTER_HOUR_PX as i32);

        let gesture = gtk::GestureClick::new();
        let cb = on_slot.clone();
        gesture.connect_released(move |g, _, _, _| {
            if let Some(widget) = g.widget() {
                cb(&widget, hour, min);
            }
        });
        marker.add_controller(gesture);

        fixed.put(&marker, 0.0, slot_y(minutes));
        slots.push(marker);
    }
    slots
}

/// Vertical position of a slot that starts `minutes` after midnight — the same
/// minutes→pixels mapping the event blocks use (`HALF_HOUR_PX` per 30 min).
fn slot_y(minutes: u32) -> f64 {
    (minutes as f64) / 30.0 * HALF_HOUR_PX
}

// ---------------------------------------------------------------------------
// Current time line
// ---------------------------------------------------------------------------

/// Add a red current-time-line to a day column, updated every 60 seconds.
/// Shared by `day_grid` (its single wide column) and `week_grid` (its 7
/// narrower columns) — `initial_width` is each grid's own placeholder,
/// resized by the tick callback below on the very next frame either way.
pub(super) fn add_current_time_line(fixed: &gtk::Fixed, initial_width: i32) {
    let line = gtk::Separator::new(gtk::Orientation::Horizontal);
    line.add_css_class("current-time-line");
    line.set_hexpand(true);
    // Initial size — will be resized by tick callback.
    line.set_size_request(initial_width, 2);

    let (hour, min) = time_utils::now_hm();
    let total_min = hour * 60 + min;
    let y = (total_min as f64) / 30.0 * HALF_HOUR_PX;
    fixed.put(&line, 0.0, y);

    // Update every 60 seconds.
    let line_clone = line.clone();
    let fixed_clone = fixed.clone();
    glib::timeout_add_local(std::time::Duration::from_secs(60), move || {
        let (h, m) = time_utils::now_hm();
        let total = h * 60 + m;
        let new_y = (total as f64) / 30.0 * HALF_HOUR_PX;
        let w = fixed_clone.width();
        if w > 0 {
            line_clone.set_size_request(w, 2);
        }
        fixed_clone.move_(&line_clone, 0.0, new_y);
        glib::ControlFlow::Continue
    });

    // Also resize line with column.
    let line_clone2 = line.clone();
    let prev_w = Rc::new(RefCell::new(0i32));
    fixed.add_tick_callback(move |widget, _| {
        let w = widget.width();
        let mut pw = prev_w.borrow_mut();
        if w > 0 && w != *pw {
            *pw = w;
            line_clone2.set_size_request(w, 2);
        }
        glib::ControlFlow::Continue
    });
}

// ---------------------------------------------------------------------------
// Hour labels column
// ---------------------------------------------------------------------------

/// Build the left column of hour labels (00:00 through 23:00).
pub fn build_hour_labels() -> gtk::Box {
    let col = gtk::Box::new(gtk::Orientation::Vertical, 0);
    col.set_size_request(56, COLUMN_HEIGHT as i32);

    for hour in 0u32..24 {
        let text = format!("{:02}:00", hour);
        let lbl = gtk::Label::new(Some(&text));
        lbl.add_css_class("hour-label");
        lbl.add_css_class("caption");
        lbl.set_halign(gtk::Align::End);
        lbl.set_valign(gtk::Align::Start);
        // Each hour = 60px (2 half-hours * 30px).
        lbl.set_size_request(-1, (2.0 * HALF_HOUR_PX) as i32);
        col.append(&lbl);
    }

    col
}

// ---------------------------------------------------------------------------
// Event collection helpers
// ---------------------------------------------------------------------------

/// Collect all-day events for the week, grouped by column index.
fn collect_all_day_events(
    week_dates: &[(i32, u32, u32)],
    events_by_date: &HashMap<String, Vec<EventRow>>,
    calendar_colors: &HashMap<String, String>,
) -> HashMap<usize, Vec<(EventRow, String)>> {
    let mut result: HashMap<usize, Vec<(EventRow, String)>> = HashMap::new();

    for (col, &(dy, dm, dd)) in week_dates.iter().enumerate() {
        let key = format!("{:04}-{:02}-{:02}", dy, dm, dd);
        if let Some(day_events) = events_by_date.get(&key) {
            for ev in day_events {
                let end_ref = ev.end_time.as_deref();
                if time_utils::is_all_day(&ev.start_time, end_ref) {
                    let color = calendar_colors
                        .get(&ev.calendar_id)
                        .cloned()
                        .unwrap_or_else(|| "slate".to_string());
                    result.entry(col).or_default().push((ev.clone(), color));
                }
            }
        }
    }

    result
}

/// Get timed (non-all-day) events for a specific date, with color and time range.
/// Returns Vec of (EventRow, color, start_minutes_from_midnight, end_minutes_from_midnight).
pub fn get_timed_events(
    date_key: &str,
    events_by_date: &HashMap<String, Vec<EventRow>>,
    calendar_colors: &HashMap<String, String>,
) -> Vec<(EventRow, String, u32, u32)> {
    let mut result = Vec::new();

    if let Some(day_events) = events_by_date.get(date_key) {
        // Shared classification + minute geometry (`fauna_core::caltime::
        // day_column_layout`, events.md § Where logic lives): a midnight-
        // crossing event renders start → 24:00 (end_min clamped to 1440) —
        // never the collapsed 30-minute block the old local derivation
        // produced by reading only the end's time-of-day.
        let inputs: Vec<(&str, Option<&str>)> = day_events
            .iter()
            .map(|ev| (ev.start_time.as_str(), ev.end_time.as_deref()))
            .collect();
        for (ev, placement) in day_events
            .iter()
            .zip(time_utils::day_column_layout(&inputs))
        {
            if let time_utils::EventPlacement::Timed {
                start_min, end_min, ..
            } = placement
            {
                let color = calendar_colors
                    .get(&ev.calendar_id)
                    .cloned()
                    .unwrap_or_else(|| "slate".to_string());
                result.push((ev.clone(), color, start_min, end_min));
            }
        }
    }

    result
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;

    /// The quick-create slots tile the full 24h column: 96 quarter-hour
    /// markers, indexed by snapped start time (ui.yaml
    /// `events-time-slot-{HH-MM}`), positioned at 1px-per-minute — and with
    /// the `set_test_id` tooltip cleared, because a tooltip popping over every
    /// pixel of empty grid space would be a UX regression, not a feature.
    #[test]
    fn slot_markers_tile_the_column_with_indexed_ids() {
        run_on_gtk_thread(|| {
            let fixed = gtk::Fixed::new();
            let slots = add_time_slot_markers(&fixed, |_, _, _| {});
            assert_eq!(slots.len(), 96);
            assert_eq!(slots[0].widget_name(), "events-time-slot-00-00");
            assert_eq!(slots[37].widget_name(), "events-time-slot-09-15");
            assert_eq!(slots[95].widget_name(), "events-time-slot-23-45");
            for marker in &slots {
                assert_eq!(
                    marker.tooltip_text(),
                    None,
                    "slot tooltip must stay cleared"
                );
            }
            // 1px per minute at the current constants: 09:15 sits at y = 555, and
            // the mapping shares HALF_HOUR_PX with the block geometry, so the two
            // cannot drift apart. (Asserted on the pure fn: gtk::Fixed's
            // child_position reads 0 until a layout pass, which a unit test
            // cannot run.)
            assert_eq!(slot_y(0), 0.0);
            assert_eq!(slot_y(9 * 60 + 15), 555.0);
            assert_eq!(slot_y(23 * 60 + 45), 1425.0);
        });
    }

    /// A slot click hands the callback the slot's OWN snapped time — the time
    /// is closure-captured at build, never derived from pointer y, so the
    /// agent's headless gesture emission (which always carries (0,0)) still
    /// names the right slot.
    #[test]
    fn slot_marker_click_reports_the_slot_time_not_the_pointer_y() {
        run_on_gtk_thread(|| {
            let fixed = gtk::Fixed::new();
            let seen: Rc<RefCell<Vec<(u32, u32)>>> = Rc::new(RefCell::new(Vec::new()));
            let s = Rc::clone(&seen);
            let slots = add_time_slot_markers(&fixed, move |_, h, m| s.borrow_mut().push((h, m)));
            let marker = &slots[37]; // 09:15
            let controllers = marker.observe_controllers();
            let mut fired = false;
            for i in 0..controllers.n_items() {
                if let Some(g) = controllers.item(i).and_downcast::<gtk::GestureClick>() {
                    g.emit_by_name::<()>("released", &[&1i32, &0.0f64, &0.0f64]);
                    fired = true;
                }
            }
            assert!(fired, "slot marker carries no GestureClick");
            assert_eq!(seen.borrow().as_slice(), &[(9u32, 15u32)]);
        });
    }

    /// A widget `put` after the markers is the LAST child: gtk::Fixed children
    /// iterate in insertion order and GTK picks last-to-first, so
    /// `build_day_column_fixed` adding blocks after the markers is what makes
    /// a click on an event block target the block, never the slot beneath it.
    /// This pins the child-order invariant that design leans on.
    #[test]
    fn a_block_added_after_the_markers_is_the_last_child() {
        run_on_gtk_thread(|| {
            let fixed = gtk::Fixed::new();
            let _slots = add_time_slot_markers(&fixed, |_, _, _| {});
            let block = gtk::Box::new(gtk::Orientation::Vertical, 0);
            crate::testid::set_test_id(&block, ids::CALENDAR_EVENT_BLOCK);
            fixed.put(&block, 0.0, 0.0);

            let mut count = 0usize;
            let mut last_name = String::new();
            let mut child = fixed.first_child();
            while let Some(c) = child {
                count += 1;
                last_name = c.widget_name().to_string();
                child = c.next_sibling();
            }
            assert_eq!(count, 97);
            assert_eq!(last_name, "calendar-event-block");
        });
    }
}
