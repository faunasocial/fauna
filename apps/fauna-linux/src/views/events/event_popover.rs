//! Quick-create popover for the events grid.
//!
//! Clicking an empty area of a month cell / week-day column opens this small
//! popover to create an event inline. The event-*detail* surface is no longer a
//! popover — it is the persistent reactive panel in `event_detail.rs` (see that
//! module's header). Event cards now select into that panel rather than opening
//! a transient preview popover.

use adw::prelude::*;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use super::calendar_view::CalendarViewState;
use super::event_form;
use crate::client::FaunaClient;
use crate::i18n::strings::{common, events as events_strings};

// ---------------------------------------------------------------------------
// Quick Create Popover
// ---------------------------------------------------------------------------

/// Show a quick-create popover anchored to `parent`.
///
/// - `date`: (year, month, day) for the new event.
/// - `time`: if `Some((hour, min))` pre-fills the start time; `None` leaves the
///   start date-only and the end blank — an all-day event.
///
/// ⚠ **No caller passes `None` today.** The month grid's day-cell double-click
/// used to, which is why this comment once read "`None` means all-day default
/// (month view)" — but that made linux the only app composing an all-day event
/// from a day cell, where tui/web/windows/macos/ios all compose at the working
/// start. It now passes `Some(caltime::WORKING_DAY_START)` like everyone else
/// (2026-08-12). The `None` arm is kept because the shape is meaningful and an
/// explicit all-day affordance would use it — not because anything is on it.
pub fn show_quick_create(
    parent: &impl IsA<gtk::Widget>,
    date: (i32, u32, u32),
    time: Option<(u32, u32)>,
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) {
    let popover = gtk::Popover::new();
    popover.set_parent(parent);
    popover.set_autohide(true);

    let vbox = gtk::Box::new(gtk::Orientation::Vertical, 6);
    vbox.set_margin_top(12);
    vbox.set_margin_bottom(12);
    vbox.set_margin_start(12);
    vbox.set_margin_end(12);
    vbox.set_width_request(260);

    // Title entry.
    let title_entry = gtk::Entry::builder()
        .placeholder_text(events_strings::SUMMARY_PLACEHOLDER)
        .build();
    title_entry.add_css_class("title-4");
    crate::testid::set_test_id(&title_entry, ids::EVENT_SUMMARY);
    vbox.append(&title_entry);

    // Start time entry.
    let (year, month, day) = date;
    let start_text = if let Some((h, m)) = time {
        format!("{:04}-{:02}-{:02} {:02}:{:02}", year, month, day, h, m)
    } else {
        format!("{:04}-{:02}-{:02}", year, month, day)
    };
    let start_entry = gtk::Entry::builder().text(&start_text).build();
    crate::testid::set_test_id(&start_entry, ids::EVENT_DTSTART);
    let start_row = labeled_row(events_strings::START, &start_entry);
    vbox.append(&start_row);

    // End time entry (start + 1 hour default, or blank for all-day).
    let end_text = if let Some((h, m)) = time {
        let end_h = if h < 23 { h + 1 } else { 23 };
        let end_m = if h < 23 { m } else { 59 };
        format!(
            "{:04}-{:02}-{:02} {:02}:{:02}",
            year, month, day, end_h, end_m
        )
    } else {
        String::new()
    };
    let end_entry = gtk::Entry::builder()
        .text(&end_text)
        .placeholder_text(events_strings::END_OPTIONAL)
        .build();
    crate::testid::set_test_id(&end_entry, ids::EVENT_DTEND);
    let end_row = labeled_row(events_strings::END, &end_entry);
    vbox.append(&end_row);

    // Calendar dropdown — every listed calendar, in sidebar order, labelled by
    // NAME. It listed `visible_calendars` by raw hex **id** before 2026-07-30:
    // unreadable, and it hid the calendar the user had just selected whenever
    // that calendar's visibility checkbox happened to be off. The dropdown
    // pre-selects the page's authoring target (events.md § Layout & flow).
    let calendar_ids: Vec<String>;
    let calendar_labels: Vec<String>;
    let preselect: usize;
    {
        let s = state.borrow();
        calendar_ids = s.displayed_calendars.iter().map(|c| c.id.clone()).collect();
        calendar_labels = s
            .displayed_calendars
            .iter()
            .map(|c| c.name.clone())
            .collect();
        preselect = s
            .authoring_target()
            .and_then(|target| calendar_ids.iter().position(|id| *id == target))
            .unwrap_or(0);
    }

    let cal_string_list = gtk::StringList::new(
        &calendar_labels
            .iter()
            .map(|s| s.as_str())
            .collect::<Vec<_>>(),
    );
    let cal_dropdown = gtk::DropDown::new(Some(cal_string_list), gtk::Expression::NONE);
    cal_dropdown.set_selected(u32::try_from(preselect).unwrap_or(0));
    let cal_row = labeled_row(events_strings::CALENDAR, &cal_dropdown);
    vbox.append(&cal_row);

    // Button row.
    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk::Align::End);
    btn_row.set_margin_top(4);

    let more_btn = gtk::Button::with_label(events_strings::MORE_OPTIONS);
    more_btn.add_css_class("flat");

    let save_btn = gtk::Button::with_label(common::SAVE);
    save_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&save_btn, ids::CREATE_EVENT);
    crate::offline_gate::declare_wire_kind(&save_btn, "fauna.bridges.put_event_ciphertext");

    btn_row.append(&more_btn);
    btn_row.append(&save_btn);
    vbox.append(&btn_row);

    popover.set_child(Some(&vbox));

    // Wire Save button.
    let cal_dropdown_for_save = cal_dropdown.clone();
    {
        let c = Rc::clone(client);
        let pop = popover.clone();
        let title_ref = title_entry.clone();
        let start_ref = start_entry.clone();
        let end_ref = end_entry.clone();
        let cal_ids = calendar_ids.clone();

        save_btn.connect_clicked(move |_| {
            let summary = title_ref.text().to_string();
            let start = start_ref.text().to_string();
            if summary.is_empty() || start.is_empty() {
                return;
            }

            let end = end_ref.text().to_string();
            let selected_idx = cal_dropdown_for_save.selected() as usize;
            let calendar_id = cal_ids.get(selected_idx).cloned().unwrap_or_default();

            let uid = format!("{}@fauna-desktop", uuid::Uuid::new_v4());
            let mut params = serde_json::json!({
                "calendar_id": calendar_id,
                "uid": uid,
                "summary": summary,
                "dtstart": start,
            });
            if !end.is_empty() {
                params["dtend"] = serde_json::Value::String(end);
            }

            c.create_event(params);
            pop.popdown();
        });
    }

    // Wire "More options" button — opens full event form with pre-filled data.
    {
        let pop = popover.clone();
        let title_ref = title_entry.clone();
        let start_ref = start_entry.clone();
        let end_ref = end_entry.clone();
        let cal_ids = calendar_ids.clone();
        let cl = Rc::clone(client);

        more_btn.connect_clicked(move |btn| {
            let selected_idx = cal_dropdown.selected() as usize;
            let calendar_id = cal_ids.get(selected_idx).cloned().unwrap_or_default();

            let data = event_form::PrefilledEventData {
                calendar_id,
                summary: title_ref.text().to_string(),
                start_time: start_ref.text().to_string(),
                end_time: end_ref.text().to_string(),
                // The quick create has no description/location inputs, and this
                // hand-off carries what the popover holds — not the rail, which
                // this compose already replaced when it opened.
                description: String::new(),
                location: String::new(),
            };

            let dialog = event_form::build_event_form_prefilled(&data, &cl);

            // Set transient for the current window.
            if let Some(root) = btn.root()
                && let Some(win) = root.downcast_ref::<gtk::Window>()
            {
                dialog.set_transient_for(Some(win));
            }
            dialog.present();
            pop.popdown();
        });
    }

    // ── The drafts rail: this gesture means *start a new event here* ─────────
    //
    // `events.md` § Persistence rules the day-cell gesture as a fresh start, so
    // opening this popover CLEARS the rail and seeds it with the cell's own date
    // prefill — the exact pair tui's `ComposeOnDay` performs
    // (`apps/fauna-tui/src/events/mod.rs`). Its three inputs then tick like the
    // full form's, so a quick create half-written here survives a restart too
    // rather than being a second-class compose (priority #1).
    {
        super::drafts::clear();
        let title_ref = title_entry.clone();
        let start_ref = start_entry.clone();
        let end_ref = end_entry.clone();
        let tick = move || {
            super::drafts::note_edit(fauna_client_caldav::drafts::EventDrafts {
                summary: title_ref.text().to_string(),
                dtstart: start_ref.text().to_string(),
                dtend: end_ref.text().to_string(),
                ..Default::default()
            });
        };
        tick();
        for entry in [&title_entry, &start_entry, &end_entry] {
            let tick = tick.clone();
            entry.connect_changed(move |_| tick());
        }
    }

    // Auto-focus the title entry after popup.
    let te = title_entry.clone();
    popover.connect_show(move |_| {
        te.grab_focus();
    });

    // Clean up parent reference when popover is closed.
    {
        let pop = popover.clone();
        popover.connect_closed(move |_| {
            pop.unparent();
        });
    }

    popover.popup();
}

// ---------------------------------------------------------------------------
// Helpers
// ---------------------------------------------------------------------------

fn labeled_row(label_text: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let label = gtk::Label::new(Some(label_text));
    label.set_width_chars(8);
    label.set_halign(gtk::Align::Start);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 6);
    row.append(&label);
    widget.set_hexpand(true);
    row.append(widget);
    row
}
