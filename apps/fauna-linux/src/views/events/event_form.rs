use adw::prelude::*;
use fauna_client_caldav::drafts::EventDrafts;
use fauna_ui_ids as ids;
use std::rc::Rc;

use super::drafts;
use crate::client::FaunaClient;
use crate::i18n::strings::{common, events as events_strings};
use crate::views::events::time_utils;

// ---------------------------------------------------------------------------
// Prefilled event form (opened from popovers)
// ---------------------------------------------------------------------------

/// Data to pre-populate the full event creation dialog.
///
/// The four text fields plus `description` / `location` are exactly the five
/// user-authored inputs the `"events"` drafts rail rests
/// (`docs/goal/ui/events.md` § Persistence) — the calendar deliberately is not
/// among them, which is why `calendar_id` is resolved by the caller from the
/// page's selection rather than carried on the draft.
pub struct PrefilledEventData {
    pub calendar_id: String,
    pub summary: String,
    pub start_time: String,
    pub end_time: String,
    pub description: String,
    pub location: String,
}

impl PrefilledEventData {
    /// Overlay a resumed draft's five fields onto a prefill, keeping the
    /// caller's date defaults for whichever datetime the draft left empty.
    ///
    /// A draft is what the user actually typed, so an empty `dtstart` means
    /// "they never touched it" — falling back to the opener's own prefill there
    /// is what keeps a resumed compose as usable as a fresh one.
    pub fn with_draft(mut self, draft: &EventDrafts) -> Self {
        self.summary = draft.summary.clone();
        self.description = draft.description.clone();
        self.location = draft.location.clone();
        if !draft.dtstart.is_empty() {
            self.start_time = draft.dtstart.clone();
        }
        if !draft.dtend.is_empty() {
            self.end_time = draft.dtend.clone();
        }
        self
    }
}

/// Build the full event form dialog with fields pre-populated from `data`.
pub fn build_event_form_prefilled(
    data: &PrefilledEventData,
    client: &Rc<FaunaClient>,
) -> adw::Window {
    let dialog = adw::Window::builder()
        .title(events_strings::NEW_EVENT)
        .default_width(420)
        .modal(true)
        .build();

    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);

    let header = adw::HeaderBar::new();
    header.set_title_widget(Some(&gtk::Label::new(Some(events_strings::NEW_EVENT))));
    outer.append(&header);

    let form = gtk::Box::new(gtk::Orientation::Vertical, 8);
    form.set_margin_top(16);
    form.set_margin_bottom(16);
    form.set_margin_start(16);
    form.set_margin_end(16);

    // Summary (pre-filled).
    let summary_entry = gtk::Entry::builder()
        .placeholder_text(events_strings::SUMMARY)
        .text(&data.summary)
        .build();
    crate::testid::set_test_id(&summary_entry, ids::EVENT_SUMMARY);
    let summary_row = labeled_row(events_strings::SUMMARY, &summary_entry);
    form.append(&summary_row);

    // Start date (pre-filled).
    let start_entry = gtk::Entry::builder()
        .placeholder_text("YYYY-MM-DDTHH:MM")
        .text(&data.start_time)
        .build();
    crate::testid::set_test_id(&start_entry, ids::EVENT_DTSTART);
    let start_row = labeled_row(events_strings::START, &start_entry);
    form.append(&start_row);

    // End date (pre-filled).
    let end_entry = gtk::Entry::builder()
        .placeholder_text("YYYY-MM-DDTHH:MM")
        .text(&data.end_time)
        .build();
    crate::testid::set_test_id(&end_entry, ids::EVENT_DTEND);
    let end_row = labeled_row(events_strings::END, &end_entry);
    form.append(&end_row);

    // Location.
    let location_entry = gtk::Entry::builder()
        .placeholder_text(events_strings::LOCATION)
        .text(&data.location)
        .build();
    crate::testid::set_test_id(&location_entry, ids::EVENT_FORM_LOCATION);
    let location_row = labeled_row(events_strings::LOCATION, &location_entry);
    form.append(&location_row);

    // Description.
    let desc_view = gtk::TextView::new();
    desc_view.set_wrap_mode(gtk::WrapMode::Word);
    desc_view.set_top_margin(8);
    desc_view.set_bottom_margin(8);
    desc_view.set_left_margin(8);
    desc_view.set_right_margin(8);
    desc_view.set_size_request(-1, 80);
    crate::testid::set_test_id(&desc_view, ids::EVENT_FORM_DESCRIPTION);
    desc_view.buffer().set_text(&data.description);

    let desc_scroll = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .child(&desc_view)
        .build();
    let desc_frame = gtk::Frame::new(Some(events_strings::DESCRIPTION));
    desc_frame.set_child(Some(&desc_scroll));
    form.append(&desc_frame);

    // ── Draft autosave (events.md § Persistence) ────────────────────────────
    //
    // Every one of the five at-rest inputs ticks the rail on change; the
    // debounce, the seal and the WS-RPC put all live behind
    // `super::drafts::note_edit`. A `TextView` has no `changed` of its own — its
    // buffer carries the signal — which is why the description hangs off
    // `TextBuffer::connect_changed` rather than the widget.
    //
    // The read is taken from the widgets at tick time rather than accumulated,
    // so the draft always mirrors what the form actually shows, including a
    // programmatic prefill the user then edited.
    {
        let sum_ref = summary_entry.clone();
        let start_ref = start_entry.clone();
        let end_ref = end_entry.clone();
        let loc_ref = location_entry.clone();
        let desc_ref = desc_view.clone();
        let tick = move || {
            drafts::note_edit(current_draft(
                &sum_ref, &start_ref, &end_ref, &loc_ref, &desc_ref,
            ));
        };
        for entry in [&summary_entry, &start_entry, &end_entry, &location_entry] {
            let tick = tick.clone();
            entry.connect_changed(move |_| tick());
        }
        desc_view.buffer().connect_changed(move |_| tick());
    }

    // Register the live widgets so a launch restore that lands *after* this
    // dialog opened still reaches the fields the user is looking at, and drop
    // the registration when the window goes away.
    drafts::attach_form(drafts::OpenForm {
        summary: summary_entry.clone(),
        dtstart: start_entry.clone(),
        dtend: end_entry.clone(),
        location: location_entry.clone(),
        description: desc_view.clone(),
    });
    dialog.connect_destroy(|_| drafts::detach_form());

    // Attendance mode dropdown.
    let mode_list = gtk::StringList::new(&[
        common::OPEN,
        events_strings::INVITE_ONLY,
        events_strings::GROUP_ONLY,
        events_strings::LINK_CODE,
    ]);
    let mode_dropdown = gtk::DropDown::new(Some(mode_list), gtk::Expression::NONE);
    mode_dropdown.set_selected(0);
    let mode_row = labeled_row(events_strings::ATTENDANCE_MODE, &mode_dropdown);
    form.append(&mode_row);

    // Capacity spin button.
    let capacity_spin = gtk::SpinButton::with_range(0.0, 10000.0, 1.0);
    capacity_spin.set_value(0.0);
    let capacity_row = labeled_row(events_strings::CAPACITY, &capacity_spin);
    form.append(&capacity_row);

    // Button row.
    let btn_row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    btn_row.set_halign(gtk::Align::End);
    btn_row.set_margin_top(8);

    let cancel_btn = gtk::Button::with_label(common::CANCEL);
    let create_btn = gtk::Button::with_label(common::CREATE);
    create_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&create_btn, ids::CREATE_EVENT);
    crate::offline_gate::declare_wire_kind(&create_btn, "fauna.bridges.put_event_ciphertext");

    btn_row.append(&cancel_btn);
    btn_row.append(&create_btn);
    form.append(&btn_row);

    outer.append(&form);
    dialog.set_content(Some(&outer));

    // Wire Cancel — and Escape, the keyboard's way to the same close. Closing
    // only detaches the form (`connect_destroy` above): the rail keeps the
    // half-written event, and New Event resumes it (`events.md` § Persistence).
    {
        let d = dialog.clone();
        cancel_btn.connect_clicked(move |_| d.close());
        let d = dialog.clone();
        let escape = gtk::ShortcutController::new();
        escape.add_shortcut(gtk::Shortcut::new(
            gtk::ShortcutTrigger::parse_string("Escape"),
            Some(gtk::CallbackAction::new(move |_, _| {
                d.close();
                gtk::glib::Propagation::Stop
            })),
        ));
        dialog.add_controller(escape);
    }

    // Wire Create.
    {
        let c = Rc::clone(client);
        let d = dialog.clone();
        let cid = data.calendar_id.clone();
        let sum_ref = summary_entry.clone();
        let start_ref = start_entry.clone();
        let end_ref = end_entry.clone();
        let loc_ref = location_entry.clone();
        let desc_ref = desc_view.clone();
        let cap_ref = capacity_spin.clone();
        let mode_ref = mode_dropdown.clone();

        create_btn.connect_clicked(move |_| {
            let summary = sum_ref.text().to_string();
            // Shared input normalization (events.md § Where logic lives): pads
            // a bare `THH:MM` with `:00` seconds — the A2-regression rule — and
            // accepts the legacy space-separated form. Sending the field raw
            // let a seconds-less value reach the nest's iCalendar serializer,
            // which emitted a compact time apple's parser dropped to midnight.
            let start = time_utils::normalize_event_datetime_input(&start_ref.text());
            if summary.is_empty() || start.is_empty() {
                return;
            }

            let end = time_utils::normalize_event_datetime_input(&end_ref.text());
            let location = loc_ref.text().to_string();
            let desc_buf = desc_ref.buffer();
            let description = desc_buf
                .text(&desc_buf.start_iter(), &desc_buf.end_iter(), false)
                .to_string();
            let capacity = cap_ref.value() as u64;
            let attendance_mode = match mode_ref.selected() {
                0 => "open",
                1 => "invite_list",
                2 => "group_only",
                _ => "link_code",
            };

            // Generate a unique UID for the event (RFC 5545 UID format).
            let uid = format!("{}@fauna-desktop", uuid::Uuid::new_v4());
            let mut params = serde_json::json!({
                "calendar_id": cid,
                "uid": uid,
                "summary": summary,
                "dtstart": start,
                "attendance_mode": attendance_mode,
            });
            if !end.is_empty() {
                params["dtend"] = serde_json::Value::String(end);
            }
            if !location.is_empty() {
                params["location"] = serde_json::Value::String(location);
            }
            if !description.is_empty() {
                params["description"] = serde_json::Value::String(description);
            }
            if capacity > 0 {
                params["capacity"] = serde_json::Value::Number(capacity.into());
            }

            c.create_event(params);
            d.close();
        });
    }

    dialog
}

/// Read the form's five at-rest inputs as this rail's record. Datetimes are
/// taken **raw as typed** — normalization stays at submit
/// (`events.md` § Persistence, and § Where logic lives' A2 rule), so a
/// half-typed value rests exactly as the user left it.
fn current_draft(
    summary: &gtk::Entry,
    start: &gtk::Entry,
    end: &gtk::Entry,
    location: &gtk::Entry,
    description: &gtk::TextView,
) -> EventDrafts {
    let buf = description.buffer();
    EventDrafts {
        summary: summary.text().to_string(),
        dtstart: start.text().to_string(),
        dtend: end.text().to_string(),
        description: buf
            .text(&buf.start_iter(), &buf.end_iter(), false)
            .to_string(),
        location: location.text().to_string(),
    }
}

fn labeled_row(label_text: &str, widget: &impl IsA<gtk::Widget>) -> gtk::Box {
    let label = gtk::Label::new(Some(label_text));
    label.set_width_chars(10);
    label.set_halign(gtk::Align::Start);

    let row = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    row.append(&label);
    widget.set_hexpand(true);
    row.append(widget);
    row
}
