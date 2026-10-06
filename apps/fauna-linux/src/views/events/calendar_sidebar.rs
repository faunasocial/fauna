use adw::prelude::*;
/// Calendar sidebar widget: scrollable list of calendars with colored
/// checkboxes for visibility toggling, plus ICS import/export buttons.
use fauna_ui_ids as ids;
use gtk::prelude::FileExt;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use super::calendar_view::CalendarViewState;
use crate::client::FaunaClient;
use crate::i18n::strings::{common, events as events_strings};
use crate::rows::CalendarRow;

// ---------------------------------------------------------------------------
// Color palette
// ---------------------------------------------------------------------------

/// 8 preset colors, assigned round-robin to calendars.
const COLORS: &[&str] = &[
    "indigo", "emerald", "amber", "rose", "cyan", "violet", "orange", "slate",
];

// ---------------------------------------------------------------------------
// Persistence
// ---------------------------------------------------------------------------

fn config_dir() -> std::path::PathBuf {
    let mut p = dirs_fallback();
    p.push("fauna");
    p
}

/// Best-effort XDG config dir without pulling in a crate.
fn dirs_fallback() -> std::path::PathBuf {
    if let Ok(xdg) = std::env::var("XDG_CONFIG_HOME") {
        return std::path::PathBuf::from(xdg);
    }
    if let Ok(home) = std::env::var("HOME") {
        let mut p = std::path::PathBuf::from(home);
        p.push(".config");
        return p;
    }
    std::path::PathBuf::from("/tmp")
}

fn colors_path() -> std::path::PathBuf {
    let mut p = config_dir();
    p.push("calendar-colors.json");
    p
}

/// Load persisted calendar→color mapping. Returns empty map on any error.
pub fn load_calendar_colors() -> HashMap<String, String> {
    let path = colors_path();
    let Ok(data) = std::fs::read_to_string(&path) else {
        return HashMap::new();
    };
    serde_json::from_str(&data).unwrap_or_default()
}

/// Save calendar→color mapping to disk. Best-effort; ignores errors.
pub fn save_calendar_colors(map: &HashMap<String, String>) {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = colors_path();
    if let Ok(json) = serde_json::to_string_pretty(map) {
        let _ = std::fs::write(&path, json);
    }
}

/// Assign colors to calendars. Reuses persisted colors when available,
/// assigns new ones round-robin for any calendar not yet in the map.
fn assign_colors(calendars: &[CalendarRow], existing: &mut HashMap<String, String>) {
    // Collect already-used color indices so we don't double-assign.
    let mut next_idx = 0usize;
    // Find the next unused color index (wrap around).
    for cal in calendars {
        if existing.contains_key(&cal.id) {
            continue;
        }
        // Pick the next color that isn't already assigned to another calendar
        // in this batch. Simple round-robin is fine.
        let color = COLORS[next_idx % COLORS.len()];
        existing.insert(cal.id.clone(), color.to_string());
        next_idx += 1;
    }
    // Advance next_idx past any already-assigned entries.
    // (Not strictly needed since we only insert for missing keys.)
}

// ---------------------------------------------------------------------------
// Public API
// ---------------------------------------------------------------------------

/// Build the calendar sidebar container and its inner `ListBox`.
///
/// The container is a vertical `gtk::Box` with a "Calendars" label, the
/// list box, and a "New Calendar" button. Call `update_calendar_sidebar`
/// after fetching calendars to populate the list.
pub fn build_calendar_sidebar(
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
    on_visibility_changed: impl Fn() + Clone + 'static,
) -> (gtk::Box, gtk::ListBox) {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 6);

    // Section label.
    let label = gtk::Label::new(Some(events_strings::CALENDARS));
    label.add_css_class("dim-label");
    label.set_halign(gtk::Align::Start);
    outer.append(&label);

    // ListBox for calendar rows.
    let list_box = gtk::ListBox::new();
    list_box.set_selection_mode(gtk::SelectionMode::None);
    list_box.add_css_class("boxed-list");

    let placeholder = gtk::Label::new(Some(events_strings::NO_CALENDARS));
    placeholder.add_css_class("dim-label");
    placeholder.set_margin_top(8);
    placeholder.set_margin_bottom(8);
    list_box.set_placeholder(Some(&placeholder));

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&list_box)
        .build();
    outer.append(&scrolled);

    // "New Calendar" button.
    let new_btn = gtk::Button::with_label(events_strings::NEW_CALENDAR);
    new_btn.add_css_class("flat");
    crate::testid::set_test_id(&new_btn, ids::NEW_CALENDAR_BTN);
    {
        let c = Rc::clone(client);
        new_btn.connect_clicked(move |btn| {
            let dialog = build_new_calendar_dialog(&c);
            if let Some(root) = btn.root()
                && let Some(win) = root.downcast_ref::<gtk::Window>()
            {
                dialog.set_transient_for(Some(win));
            }
            dialog.present();
        });
    }
    outer.append(&new_btn);

    // ICS Import/Export buttons row.
    let ics_row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    ics_row.set_margin_top(4);

    // --- Export ICS button ---
    let export_btn = gtk::Button::with_label(events_strings::EXPORT_ICS);
    export_btn.add_css_class("flat");
    export_btn.set_hexpand(true);
    export_btn.set_tooltip_text(Some(events_strings::EXPORT_ICS_TOOLTIP));
    {
        let lb = list_box.clone();
        let c = Rc::clone(client);
        let st = Rc::clone(state);
        export_btn.connect_clicked(move |btn| {
            // Export is scoped to the currently selected calendar
            // (goal/ui/events.md § Import / Export) — `authoring_target()`
            // resolves that, falling back to the first visible / first listed
            // calendar when nothing is selected.
            let cal_id = st.borrow().authoring_target();
            let cal_id = match cal_id {
                Some(id) => id,
                None => {
                    // Try first calendar in the list box.
                    if let Some(row) = lb.row_at_index(0) {
                        row.widget_name().to_string()
                    } else {
                        return; // No calendars
                    }
                }
            };
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            show_export_dialog(window.as_ref(), &cal_id, &c);
        });
    }
    ics_row.append(&export_btn);

    // --- Import ICS button ---
    let import_btn = gtk::Button::with_label(events_strings::IMPORT_ICS);
    import_btn.add_css_class("flat");
    import_btn.set_hexpand(true);
    import_btn.set_tooltip_text(Some(events_strings::IMPORT_ICS_TOOLTIP));
    {
        let lb = list_box.clone();
        let c = Rc::clone(client);
        let st = Rc::clone(state);
        import_btn.connect_clicked(move |btn| {
            // Import lands in the currently selected calendar — same rule as
            // export above (goal/ui/events.md § Import / Export).
            let cal_id = st.borrow().authoring_target();
            let cal_id = match cal_id {
                Some(id) => id,
                None => {
                    if let Some(row) = lb.row_at_index(0) {
                        row.widget_name().to_string()
                    } else {
                        return;
                    }
                }
            };
            let window = btn.root().and_then(|r| r.downcast::<gtk::Window>().ok());
            show_import_dialog(window.as_ref(), &cal_id, &c);
        });
    }
    ics_row.append(&import_btn);

    outer.append(&ics_row);

    // Initial population (empty — will be filled by update_calendar_sidebar).
    let _ = state;
    let _ = on_visibility_changed;

    (outer, list_box)
}

/// Re-populate the calendar sidebar list from `calendars`.
///
/// Assigns colors (persisted), updates `state.calendar_colors` and
/// `state.visible_calendars`, and wires checkbox toggles to
/// `on_visibility_changed`.
pub fn update_calendar_sidebar(
    list_box: &gtk::ListBox,
    calendars: &[CalendarRow],
    state: &Rc<RefCell<CalendarViewState>>,
    on_visibility_changed: impl Fn() + Clone + 'static,
    on_select: impl Fn(String) + Clone + 'static,
) {
    // Clear existing rows.
    while let Some(child) = list_box.first_child() {
        list_box.remove(&child);
    }

    // Assign colors.
    let mut colors = load_calendar_colors();
    assign_colors(calendars, &mut colors);
    save_calendar_colors(&colors);

    // Update shared state.
    {
        let mut s = state.borrow_mut();
        // Snapshot old colors to detect truly new calendars (before updating).
        let old_colors = std::mem::replace(&mut s.calendar_colors, colors.clone());
        // Ensure all calendars start visible (add new ones; keep existing
        // visibility choices if the user unchecked something).
        for cal in calendars {
            // On first load all are visible; on subsequent loads keep existing
            // state. We add only if the set is empty (first load) or if this
            // is a brand-new calendar (not in the OLD color map).
            if s.visible_calendars.is_empty() || !old_colors.contains_key(&cal.id) {
                s.visible_calendars.insert(cal.id.clone());
            }
        }
        // On very first load (set was empty), add all.
        if s.visible_calendars.is_empty() {
            for cal in calendars {
                s.visible_calendars.insert(cal.id.clone());
            }
        }
    }

    // Build a row for each calendar.
    for cal in calendars {
        let color = colors.get(&cal.id).map(|s| s.as_str()).unwrap_or("slate");

        let (is_visible, is_selected) = {
            let s = state.borrow();
            (
                s.visible_calendars.contains(&cal.id),
                s.selected_calendar.as_deref() == Some(cal.id.as_str()),
            )
        };

        let row = build_sidebar_row(
            cal,
            color,
            is_visible,
            is_selected,
            state,
            on_visibility_changed.clone(),
            on_select.clone(),
        );
        list_box.append(&row);
    }
}

/// Repaint the `cal-selected` marker across the sidebar's `calendar-item`
/// buttons without rebuilding a single row.
///
/// A selection click must not destroy/recreate the `calendar-item` widgets it
/// was just delivered to (`calendar_view.rs`'s poll comment — a rebuild under
/// the cursor both flickers and races the automation agent's element lookup),
/// so the click handler restyles in place instead of re-running
/// [`update_calendar_sidebar`].
pub fn mark_selected_calendar(list_box: &gtk::ListBox, selected: Option<&str>) {
    let mut index = 0;
    while let Some(row) = list_box.row_at_index(index) {
        index += 1;
        let is_selected = Some(row.widget_name().as_str()) == selected;
        let Some(name_btn) = find_calendar_item_button(row.upcast_ref::<gtk::Widget>()) else {
            continue;
        };
        if is_selected {
            name_btn.add_css_class("cal-selected");
        } else {
            name_btn.remove_css_class("cal-selected");
        }
    }
}

/// The row's `calendar-item` button — the row's own widget name is the calendar
/// id (the import/export fallback reads it), so match on the test id instead.
fn find_calendar_item_button(widget: &gtk::Widget) -> Option<gtk::Button> {
    if widget.widget_name() == "calendar-item" {
        return widget.clone().downcast::<gtk::Button>().ok();
    }
    let mut child = widget.first_child();
    while let Some(c) = child {
        if let Some(found) = find_calendar_item_button(&c) {
            return Some(found);
        }
        child = c.next_sibling();
    }
    None
}

// ---------------------------------------------------------------------------
// Internal helpers
// ---------------------------------------------------------------------------

/// Build a single sidebar row: colored dot + name (the `calendar-item` select
/// button) + the `calendar-visibility` check button.
fn build_sidebar_row(
    cal: &CalendarRow,
    color: &str,
    visible: bool,
    selected: bool,
    state: &Rc<RefCell<CalendarViewState>>,
    on_visibility_changed: impl Fn() + Clone + 'static,
    on_select: impl Fn(String) + Clone + 'static,
) -> gtk::ListBoxRow {
    let cal_id = cal.id.as_str();
    let name = cal.name.as_str();
    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(6);
    hbox.set_margin_bottom(6);
    hbox.set_margin_start(8);
    hbox.set_margin_end(8);

    // Colored dot.
    let dot = gtk::Label::new(Some("\u{25CF}"));
    let bg_class = format!("cal-{}-text", color);
    dot.add_css_class(&bg_class);
    dot.set_margin_end(2);
    hbox.append(&dot);

    // Calendar name — a flat button, because `calendar-item` is the SELECT
    // affordance (goal/ui/events.md § User actions: "Select calendar (filter
    // visible events)"), the same click every other app wires it to. It was a
    // bare `gtk::Label` until 2026-07-30, which made the shared
    // `EventsActions.select_calendar()` a silent no-op here: the automation
    // agent actuates a click via `activate()`, and a Label has no activation.
    // A Button also keeps `find::text_of` returning exactly the calendar name.
    //
    // Built with `with_label` rather than a custom child: `find::text_of` reads
    // a Button's own `label` property, which GTK only serves while the button
    // still owns its internal label child — a `set_child` would make the
    // driver's `get_text("calendar-item")` read back empty. Style that internal
    // label in place instead.
    let name_btn = gtk::Button::with_label(name);
    name_btn.set_hexpand(true);
    name_btn.add_css_class("flat");
    if let Some(lbl) = name_btn.child().and_downcast::<gtk::Label>() {
        lbl.set_halign(gtk::Align::Start);
        lbl.set_ellipsize(gtk::pango::EllipsizeMode::End);
    }
    crate::testid::set_test_id(&name_btn, ids::CALENDAR_ITEM);
    if selected {
        name_btn.add_css_class("cal-selected");
    }
    {
        let id = cal_id.to_string();
        let on_select = on_select.clone();
        name_btn.connect_clicked(move |_| on_select(id.clone()));
    }
    hbox.append(&name_btn);

    // Visibility check button.
    let check = gtk::CheckButton::new();
    check.set_active(visible);
    crate::testid::set_test_id(&check, ids::CALENDAR_VISIBILITY);
    {
        let st = Rc::clone(state);
        let id = cal_id.to_string();
        let cb = on_visibility_changed.clone();
        check.connect_toggled(move |btn| {
            let mut s = st.borrow_mut();
            if btn.is_active() {
                s.visible_calendars.insert(id.clone());
            } else {
                s.visible_calendars.remove(&id);
            }
            drop(s);
            cb();
        });
    }
    hbox.append(&check);

    let row = gtk::ListBoxRow::new();
    row.set_widget_name(cal_id);
    row.set_child(Some(&hbox));
    row.set_activatable(false);
    row
}

/// Show a GTK FileDialog for saving an .ics export.
fn show_export_dialog(window: Option<&gtk::Window>, calendar_id: &str, client: &Rc<FaunaClient>) {
    let dialog = gtk::FileDialog::builder()
        .title(events_strings::EXPORT_CALENDAR_TITLE)
        .initial_name("calendar.ics")
        .build();

    // Add .ics filter.
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("iCalendar files (*.ics)"));
    filter.add_pattern("*.ics");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    let c = Rc::clone(client);
    let cal_id = calendar_id.to_string();
    let win = window.cloned();
    dialog.save(win.as_ref(), gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result
            && let Some(path) = file.path()
        {
            let path_str = path.to_string_lossy().to_string();
            c.export_calendar_ics(&cal_id, &path_str);
        }
    });
}

/// Show a GTK FileDialog for picking an .ics file to import.
fn show_import_dialog(window: Option<&gtk::Window>, calendar_id: &str, client: &Rc<FaunaClient>) {
    let dialog = gtk::FileDialog::builder()
        .title(events_strings::IMPORT_CALENDAR_TITLE)
        .build();

    // Add .ics filter.
    let filter = gtk::FileFilter::new();
    filter.set_name(Some("iCalendar files (*.ics)"));
    filter.add_pattern("*.ics");
    let filters = gio::ListStore::new::<gtk::FileFilter>();
    filters.append(&filter);
    dialog.set_filters(Some(&filters));
    dialog.set_default_filter(Some(&filter));

    let c = Rc::clone(client);
    let cal_id = calendar_id.to_string();
    let win = window.cloned();
    dialog.open(win.as_ref(), gio::Cancellable::NONE, move |result| {
        if let Ok(file) = result
            && let Some(path) = file.path()
        {
            match std::fs::read(&path) {
                Ok(data) => c.import_calendar_ics(&cal_id, data),
                Err(e) => {
                    tracing::error!("Failed to read ICS file: {e}");
                }
            }
        }
    });
}

/// Build a transient dialog for creating a new calendar.
fn build_new_calendar_dialog(client: &Rc<FaunaClient>) -> adw::Window {
    let dialog = adw::Window::builder()
        .title(events_strings::NEW_CALENDAR)
        .default_width(340)
        .modal(true)
        .build();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(events_strings::NEW_CALENDAR))));
    outer.append(&header);

    let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
    form.set_margin_top(16);
    form.set_margin_bottom(16);
    form.set_margin_start(16);
    form.set_margin_end(16);

    let name_entry = gtk::Entry::builder()
        .placeholder_text(events_strings::CALENDAR_NAME)
        .build();
    crate::testid::set_test_id(&name_entry, ids::CALENDAR_NAME);
    form.append(&name_entry);

    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk::Align::End);
    btn_row.set_margin_top(8);

    let cancel_btn = gtk::Button::with_label(common::CANCEL);
    let create_btn = gtk::Button::with_label(common::CREATE);
    create_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&create_btn, ids::CREATE_CALENDAR);
    crate::offline_gate::declare_wire_kind(&create_btn, "fauna.bridges.provision_calendar");

    btn_row.append(&cancel_btn);
    btn_row.append(&create_btn);
    form.append(&btn_row);
    outer.append(&form);
    dialog.set_content(Some(&outer));

    // Wire Cancel.
    {
        let d = dialog.clone();
        cancel_btn.connect_clicked(move |_| d.close());
    }
    // Wire Create.
    {
        let c = Rc::clone(client);
        let d = dialog.clone();
        let entry_ref = name_entry.clone();
        create_btn.connect_clicked(move |_| {
            let name = entry_ref.text().to_string();
            if !name.is_empty() {
                c.create_calendar(&name, "private");
                d.close();
            }
        });
    }

    dialog
}
