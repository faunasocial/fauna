use adw::prelude::*;
use fauna_core::rsvp::RsvpResponse;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::collections::BTreeMap;
use std::rc::Rc;

use super::calendar_view::CalendarViewState;
use crate::client::FaunaClient;
use crate::rows::EventRow;

/// Build one agenda event row.
///
/// The summary is a clickable `gtk::Button` carrying the `event-card` ID (a
/// Button, not a Label, so AT-SPI `do_action(0)` fires the click) that selects
/// the event in the persistent detail panel — mirroring the month grid's event
/// bars and goal/ui/events.md ("Event cards clickable → event_detail"). The
/// inline RSVP buttons (`event-rsvp-*`) stay alongside per ui.yaml's
/// `rsvp-button-group` (canonical inside `event-card`).
fn build_event_row_wired(event: &EventRow, client: &Rc<FaunaClient>) -> gtk::ListBoxRow {
    // Clickable summary → event-detail popover.
    let summary_btn = gtk::Button::with_label(&event.summary);
    summary_btn.set_halign(gtk::Align::Start);
    summary_btn.set_hexpand(true);
    summary_btn.add_css_class("flat");
    summary_btn.add_css_class("heading");
    crate::testid::set_test_id(&summary_btn, ids::EVENT_CARD);
    {
        let ev = event.clone();
        let cl = Rc::clone(client);
        summary_btn.connect_clicked(move |_| {
            cl.select_event(Some(ev.clone()));
            cl.fetch_attendees(&ev.calendar_id, &ev.id);
        });
    }

    // Hidden marker carrying the canonical `event-card-summary` ID for get_text().
    // Height >= 1px so AT-SPI reports SHOWING.
    let summary_marker = gtk::Label::new(Some(&event.summary));
    summary_marker.set_height_request(1);
    summary_marker.set_overflow(gtk::Overflow::Hidden);
    crate::testid::set_test_id(&summary_marker, ids::EVENT_CARD_SUMMARY);

    let date_label = gtk::Label::new(Some(&event.start_time));
    date_label.set_halign(gtk::Align::End);
    date_label.add_css_class("dim-label");
    date_label.add_css_class("caption");

    // Inline RSVP buttons (canonical per ui.yaml `rsvp-button-group`, inside event-card).
    //
    // Each submits through `event_detail::wire_rsvp`, the same helper the detail
    // panel's `event-detail-rsvp-*` trio uses — `rsvp-button-group` is a child of
    // BOTH surfaces (events.md § components), so they must not grow separate
    // submit paths. They shipped here as pure decoration: built, ID-tagged and
    // packed, but never `connect_clicked`. A `gtk::Button` *is* activatable, so
    // the agent's click reported `{"ok": true}` and the RSVP silently did
    // nothing — for a real user too, not just the harness. Caught by
    // `test_event_card_rsvp[linux]`, which its detail-panel sibling
    // `test_event_rsvp[linux]` passed right beside for as long as it was red.
    let going_btn = gtk::Button::with_label(crate::i18n::strings::events::rsvp::GOING);
    going_btn.add_css_class("flat");
    crate::testid::set_test_id(&going_btn, ids::EVENT_RSVP_GOING);
    crate::offline_gate::declare_wire_kind(&going_btn, "fauna.bridges.put_event_ciphertext");
    super::event_detail::wire_rsvp(
        &going_btn,
        client,
        &event.calendar_id,
        &event.id,
        RsvpResponse::Going,
    );

    let interested_btn = gtk::Button::with_label(crate::i18n::strings::events::rsvp::INTERESTED);
    interested_btn.add_css_class("flat");
    crate::testid::set_test_id(&interested_btn, ids::EVENT_RSVP_INTERESTED);
    crate::offline_gate::declare_wire_kind(&interested_btn, "fauna.bridges.put_event_ciphertext");
    super::event_detail::wire_rsvp(
        &interested_btn,
        client,
        &event.calendar_id,
        &event.id,
        RsvpResponse::Interested,
    );

    let decline_btn = gtk::Button::with_label(crate::i18n::strings::events::rsvp::DECLINE);
    decline_btn.add_css_class("flat");
    crate::testid::set_test_id(&decline_btn, ids::EVENT_RSVP_DECLINE);
    crate::offline_gate::declare_wire_kind(&decline_btn, "fauna.bridges.put_event_ciphertext");
    super::event_detail::wire_rsvp(
        &decline_btn,
        client,
        &event.calendar_id,
        &event.id,
        RsvpResponse::Declined,
    );

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(8);
    hbox.set_margin_bottom(8);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&summary_btn);
    hbox.append(&summary_marker);
    hbox.append(&date_label);
    hbox.append(&going_btn);
    hbox.append(&interested_btn);
    hbox.append(&decline_btn);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row
}

// ---------------------------------------------------------------------------
// Agenda view
// ---------------------------------------------------------------------------

/// Build the agenda scrollable container. `refresh_agenda_view` does the actual
/// render (and re-render), rebuilding the scroll + inner box wholesale each call
/// — mirroring `refresh_month_grid`. (The previous design navigated back into the
/// ScrolledWindow to find the inner box on refresh, but GTK4 wraps a
/// non-scrollable Box child in an auto-inserted GtkViewport, so that downcast
/// always failed and the agenda never re-rendered after its initial build.)
pub fn build_agenda_view(
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_vexpand(true);
    outer.set_hexpand(true);
    refresh_agenda_view(&outer, state, client);
    outer
}

/// Re-render the agenda view from `state.events_cache`, filtered by
/// `state.visible_calendars`, grouped by date. Clears `container` and rebuilds
/// the ScrolledWindow + inner box each call (mirrors `refresh_month_grid`).
pub fn refresh_agenda_view(
    container: &gtk::Box,
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) {
    while let Some(child) = container.first_child() {
        container.remove(&child);
    }

    let scroll = gtk::ScrolledWindow::new();
    scroll.set_vexpand(true);
    scroll.set_hexpand(true);
    scroll.set_policy(gtk::PolicyType::Never, gtk::PolicyType::Automatic);

    let inner = gtk::Box::new(gtk::Orientation::Vertical, 0);
    inner.set_margin_top(8);
    inner.set_margin_bottom(8);
    inner.set_margin_start(12);
    inner.set_margin_end(12);
    inner.set_widget_name("agenda-inner");

    populate_agenda_inner(&inner, state, client);

    scroll.set_child(Some(&inner));
    container.append(&scroll);
}

/// Clear and repopulate the inner agenda box.
fn populate_agenda_inner(
    inner: &gtk::Box,
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) {
    // Remove all existing children.
    while let Some(child) = inner.first_child() {
        inner.remove(&child);
    }

    let s = state.borrow();

    // Filter to the page's current calendar scope: a live `calendar-item`
    // selection, else the `calendar-visibility` checkboxes over the union.
    let visible: Vec<&crate::rows::EventRow> = s
        .events_cache
        .iter()
        .filter(|e| s.shows_calendar(&e.calendar_id))
        .collect();

    if visible.is_empty() {
        let placeholder = adw::StatusPage::builder()
            .icon_name("view-list-symbolic")
            .title(crate::i18n::strings::events::NO_UPCOMING_EVENTS)
            .description(crate::i18n::strings::events::NO_UPCOMING_EVENTS_DESC)
            .build();
        placeholder.set_vexpand(true);
        inner.append(&placeholder);
        return;
    }

    // Group by date prefix (first 10 chars of start_time: "YYYY-MM-DD").
    let mut by_date: BTreeMap<String, Vec<&crate::rows::EventRow>> = BTreeMap::new();
    for event in &visible {
        let date_key = event
            .start_time
            .get(..10)
            .unwrap_or(&event.start_time)
            .to_string();
        by_date.entry(date_key).or_default().push(event);
    }

    for (date, events) in &by_date {
        // Date header label.
        let header = gtk::Label::new(Some(date));
        header.set_halign(gtk::Align::Start);
        header.add_css_class("heading");
        header.set_margin_top(12);
        header.set_margin_bottom(4);
        inner.append(&header);

        // Event rows for this date.
        let list_box = gtk::ListBox::new();
        list_box.set_selection_mode(gtk::SelectionMode::None);
        list_box.add_css_class("boxed-list");

        for event in events {
            let row = build_event_row_wired(event, client);
            row.set_widget_name(&event.id);
            list_box.append(&row);
        }

        inner.append(&list_box);
    }
}
