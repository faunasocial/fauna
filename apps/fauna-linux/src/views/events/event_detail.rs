//! The persistent event-detail panel — the right column of the events view.
//!
//! This is a **stable, observer-driven** surface (goal/ui/events.md arch rule #1:
//! "Observer-driven rendering"), matching web/macOS which render event detail in
//! a reactive panel. It replaces the former autohide `gtk::Popover`
//! (`event_popover::show_quick_preview`), which rendered attendees from a
//! build-time snapshot and dismissed on any outside click — so a freshly-RSVP'd
//! attendee never appeared until the popover was re-opened, and headless AT-SPI
//! could not reliably drive it.
//!
//! Reactivity: clicking an event card in any view sends `EventSelected`, which
//! sets `CalendarViewState::selected_event` and calls `refresh_event_detail_panel`.
//! When `EventAttendeesLoaded` later arrives for the selected event (e.g. after an
//! RSVP triggers `fetch_attendees`), the panel re-renders from the updated cache.

use adw::prelude::*;
use fauna_core::rsvp::RsvpResponse;
use fauna_ui_ids as ids;
use std::cell::RefCell;
use std::rc::Rc;

use super::caldav_backend::CalDavAttendee;
use super::calendar_view::CalendarViewState;
use crate::client::FaunaClient;
use crate::i18n::strings::events as events_strings;
use crate::rows::EventRow;

const PANEL_WIDTH: i32 = 340;

/// Build the persistent detail panel container and render its initial state
/// (empty, since no event is selected at startup).
pub fn build_detail_panel(
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    let panel = gtk::Box::new(gtk::Orientation::Vertical, 0);
    panel.set_width_request(PANEL_WIDTH);
    refresh_event_detail_panel(&panel, state, client);
    panel
}

/// Clear and rebuild the detail panel from `state.selected_event` +
/// `state.attendees_cache`. Shows the empty state when nothing is selected.
/// Called whenever the selection changes (`EventSelected`) or attendees arrive
/// for the selected event (`EventAttendeesLoaded`).
pub fn refresh_event_detail_panel(
    panel: &gtk::Box,
    state: &Rc<RefCell<CalendarViewState>>,
    client: &Rc<FaunaClient>,
) {
    while let Some(child) = panel.first_child() {
        panel.remove(&child);
    }

    let (event, attendees, reminder) = {
        let s = state.borrow();
        match s.selected_event.clone() {
            Some(ev) => {
                let att = s.attendees_cache.get(&ev.id).cloned().unwrap_or_default();
                // `None` outer = reminder not loaded yet (get_reminder in flight);
                // `Some(None)` = loaded, no reminder set; `Some(Some(off))` = set.
                let rem = s.reminders_cache.get(&ev.id).cloned();
                (ev, att, rem)
            }
            None => {
                panel.append(&build_empty_detail());
                return;
            }
        }
    };

    panel.append(&build_detail_content(&event, &attendees, reminder, client));
}

/// The empty-state shown when no event is selected.
fn build_empty_detail() -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_vexpand(true);

    let status = adw::StatusPage::builder()
        .title(events_strings::NO_EVENT_SELECTED)
        .description(events_strings::SELECT_EVENT_HINT)
        .icon_name("x-office-calendar-symbolic")
        .vexpand(true)
        .build();

    outer.append(&status);
    outer
}

/// Build the detail content for `event`, rendering `attendees` from the cache.
/// All buttons are wired to `client`; RSVP and delete trigger the message-loop
/// refresh that re-renders this panel.
fn build_detail_content(
    event: &EventRow,
    attendees: &[CalDavAttendee],
    reminder: Option<Option<String>>,
    client: &Rc<FaunaClient>,
) -> gtk::Box {
    let outer = gtk::Box::new(gtk::Orientation::Vertical, 0);
    outer.set_vexpand(true);

    let content = gtk::Box::new(gtk::Orientation::Vertical, 8);
    content.set_margin_top(24);
    content.set_margin_bottom(24);
    content.set_margin_start(24);
    content.set_margin_end(24);

    // Summary.
    let summary_label = gtk::Label::new(Some(&event.summary));
    summary_label.set_halign(gtk::Align::Start);
    summary_label.set_wrap(true);
    summary_label.add_css_class("title-2");
    crate::testid::set_test_id(&summary_label, ids::EVENT_DETAIL_SUMMARY);
    content.append(&summary_label);

    // Time range — `event-detail-time` on the start label (Linux-only ID).
    let start_label = gtk::Label::new(Some(&format!(
        "{}: {}",
        events_strings::START,
        event.start_time
    )));
    start_label.set_halign(gtk::Align::Start);
    start_label.add_css_class("dim-label");
    crate::testid::set_test_id(&start_label, ids::EVENT_DETAIL_TIME);
    content.append(&start_label);

    if let Some(ref end) = event.end_time
        && !end.is_empty()
    {
        let end_label = gtk::Label::new(Some(&format!("{}: {}", events_strings::END, end)));
        end_label.set_halign(gtk::Align::Start);
        end_label.add_css_class("dim-label");
        content.append(&end_label);
    }

    // Location.
    if let Some(ref location) = event.location
        && !location.is_empty()
    {
        let loc_section = section_heading(events_strings::LOCATION);
        content.append(&loc_section);
        let loc_label = gtk::Label::new(Some(location));
        loc_label.set_halign(gtk::Align::Start);
        loc_label.set_wrap(true);
        crate::testid::set_test_id(&loc_label, ids::EVENT_DETAIL_LOCATION);
        content.append(&loc_label);
    }

    // Description.
    if let Some(ref description) = event.description
        && !description.is_empty()
    {
        let desc_section = section_heading(events_strings::DESCRIPTION);
        content.append(&desc_section);
        let desc_label = gtk::Label::new(Some(description));
        desc_label.set_halign(gtk::Align::Start);
        desc_label.set_wrap(true);
        crate::testid::set_test_id(&desc_label, ids::EVENT_DETAIL_DESCRIPTION);
        content.append(&desc_label);
    }

    // RSVP buttons.
    content.append(&section_heading(events_strings::rsvp::TITLE));
    let rsvp_box = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    rsvp_box.set_margin_top(4);

    let going_btn = gtk::Button::with_label(events_strings::rsvp::GOING);
    going_btn.add_css_class("suggested-action");
    crate::testid::set_test_id(&going_btn, ids::EVENT_DETAIL_RSVP_GOING);
    crate::offline_gate::declare_wire_kind(&going_btn, "fauna.bridges.put_event_ciphertext");
    wire_rsvp(
        &going_btn,
        client,
        &event.calendar_id,
        &event.id,
        RsvpResponse::Going,
    );
    rsvp_box.append(&going_btn);

    let interested_btn = gtk::Button::with_label(events_strings::rsvp::INTERESTED);
    crate::testid::set_test_id(&interested_btn, ids::EVENT_DETAIL_RSVP_INTERESTED);
    crate::offline_gate::declare_wire_kind(&interested_btn, "fauna.bridges.put_event_ciphertext");
    wire_rsvp(
        &interested_btn,
        client,
        &event.calendar_id,
        &event.id,
        RsvpResponse::Interested,
    );
    rsvp_box.append(&interested_btn);

    let decline_btn = gtk::Button::with_label(crate::i18n::strings::common::DECLINE);
    decline_btn.add_css_class("destructive-action");
    crate::testid::set_test_id(&decline_btn, ids::EVENT_DETAIL_RSVP_DECLINE);
    crate::offline_gate::declare_wire_kind(&decline_btn, "fauna.bridges.put_event_ciphertext");
    wire_rsvp(
        &decline_btn,
        client,
        &event.calendar_id,
        &event.id,
        RsvpResponse::Declined,
    );
    rsvp_box.append(&decline_btn);

    content.append(&rsvp_box);

    // Attendees — rendered reactively from the cache; each carries `attendee-item`.
    content.append(&section_heading(events_strings::ATTENDEES));
    let attendee_list = gtk::ListBox::new();
    attendee_list.set_selection_mode(gtk::SelectionMode::None);
    attendee_list.add_css_class("boxed-list");
    crate::testid::set_test_id(&attendee_list, ids::ATTENDEE_LIST);

    if attendees.is_empty() {
        let placeholder = gtk::Label::new(Some(events_strings::NO_ATTENDEES));
        placeholder.add_css_class("dim-label");
        placeholder.set_margin_top(8);
        placeholder.set_margin_bottom(8);
        attendee_list.set_placeholder(Some(&placeholder));
    } else {
        for attendee in attendees {
            attendee_list.append(&build_attendee_row(attendee));
        }
    }
    content.append(&attendee_list);

    // Invite an attendee (events.md § Scheduling & invitations). The
    // `attendee-invite-field` email entry is the universal attendee mechanism —
    // a `mailto:` CAL-ADDRESS that is handle == email == Fauna identity in
    // production (caldav-server.md § Scheduling & invitations). On Invite the
    // typed email is added to the event's roster (re-PUT) and an iMIP REQUEST is
    // fanned out to the email-reachable attendees via the shared scheduling
    // crate + the outbound mail path. Cross-nest mailbox-less-Fauna delivery is
    // fully automatic — resolved from the typed CAL-ADDRESS alone via anon
    // by_handle discovery (`resolve_attendee_transport`), no manual nest URL.
    let invite_email = gtk::Entry::builder()
        .placeholder_text(events_strings::invite::EMAIL_PLACEHOLDER)
        .build();
    invite_email.set_margin_top(8);
    crate::testid::set_test_id(&invite_email, ids::ATTENDEE_INVITE_FIELD);
    content.append(&invite_email);

    let invite_btn = gtk::Button::with_label(events_strings::invite::BUTTON);
    invite_btn.add_css_class("flat");
    crate::testid::set_test_id(&invite_btn, ids::ATTENDEE_INVITE_BUTTON);
    crate::offline_gate::declare_wire_kind(&invite_btn, "fauna.bridges.put_event_ciphertext");
    {
        let c = Rc::clone(client);
        let eid = event.id.clone();
        let cal = event.calendar_id.clone();
        let email_entry = invite_email.clone();
        invite_btn.connect_clicked(move |_| {
            // `eid` is the hex `uid_hash` (EventRow::id); `cal` the calendar id.
            // The typed email is added to the roster before the fan-out; an empty
            // field re-sends to the existing roster.
            let email = email_entry.text().to_string();
            c.invite_to_event(&cal, &eid, &email);
            email_entry.set_text("");
        });
    }
    content.append(&invite_btn);

    // Reminder (the shared `event-reminder` component — ui.yaml). Two states:
    // when a reminder is set, the current-offset label + a Remove button; when
    // not, a preset <DropDown> + a Set button. `reminder == None` means the
    // get_reminder load is still in flight (render the unset controls; the panel
    // re-renders via EventReminderLoaded once it resolves). The DropDown rides
    // the shared `reminder_presets()` catalog with the  value/label split:
    // the MODEL strings are the stable ISO values so the cross-app
    // `select(id, "PT1H")` contract matches by value, and a display expression
    // maps each to its localized `reminder_label` phrase.
    content.append(&section_heading(events_strings::reminder::TITLE));
    match reminder.flatten() {
        Some(offset) => {
            // The preset → human-label map is shared Rust (`fauna_core::ical::
            // reminder_label`, events.md § Reminders) so every app derives the
            // same label; linux links `fauna_core` directly (no FFI) and resolves
            // the i18n key through its own lookup. Non-preset offsets render
            // verbatim (the LocalizedText key falls back to the raw value).
            let reminder_text =
                fauna_core::ical::reminder_label(&offset).resolve(crate::i18n::strings::lookup);
            let current = gtk::Label::new(Some(reminder_text.as_str()));
            current.set_halign(gtk::Align::Start);
            crate::testid::set_test_id(&current, ids::EVENT_DETAIL_REMINDER_CURRENT);
            content.append(&current);

            let remove_btn = gtk::Button::with_label(crate::i18n::strings::common::REMOVE);
            remove_btn.add_css_class("flat");
            crate::testid::set_test_id(&remove_btn, ids::EVENT_DETAIL_REMINDER_REMOVE);
            crate::offline_gate::declare_wire_kind(
                &remove_btn,
                "fauna.bridges.put_event_ciphertext",
            );
            {
                let c = Rc::clone(client);
                let cal = event.calendar_id.clone();
                let eid = event.id.clone();
                remove_btn.connect_clicked(move |_| {
                    c.remove_reminder(&cal, &eid);
                });
            }
            content.append(&remove_btn);
        }
        None => {
            let presets = fauna_core::ical::reminder_presets();
            let preset_values: Vec<&str> = presets.iter().map(|p| p.value.as_str()).collect();
            let model = gtk::StringList::new(&preset_values);
            let dropdown = gtk::DropDown::new(Some(model), gtk::Expression::NONE);
            let label_expr = gtk::ClosureExpression::new::<String>(
                &[] as &[gtk::Expression],
                gtk::glib::closure!(|item: gtk::StringObject| {
                    fauna_core::ical::reminder_label(item.string().as_str())
                        .resolve(crate::i18n::strings::lookup)
                }),
            );
            dropdown.set_expression(Some(&label_expr));
            crate::testid::set_test_id(&dropdown, ids::EVENT_DETAIL_REMINDER_SELECT);
            content.append(&dropdown);

            let set_btn = gtk::Button::with_label(events_strings::reminder::SET);
            set_btn.add_css_class("flat");
            crate::testid::set_test_id(&set_btn, ids::EVENT_DETAIL_REMINDER_SET);
            crate::offline_gate::declare_wire_kind(&set_btn, "fauna.bridges.put_event_ciphertext");
            {
                let c = Rc::clone(client);
                let cal = event.calendar_id.clone();
                let eid = event.id.clone();
                let dd = dropdown.clone();
                set_btn.connect_clicked(move |_| {
                    let offset = dd
                        .selected_item()
                        .and_then(|o| o.downcast::<gtk::StringObject>().ok())
                        .map(|s| s.string().to_string())
                        .unwrap_or_default();
                    if !offset.is_empty() {
                        c.set_reminder(&cal, &eid, &offset);
                    }
                });
            }
            content.append(&set_btn);
        }
    }

    // Delete — clears the panel selection so it doesn't show the gone event.
    let delete_btn = gtk::Button::with_label(events_strings::DELETE_EVENT);
    delete_btn.add_css_class("destructive-action");
    delete_btn.set_margin_top(16);
    crate::testid::set_test_id(&delete_btn, ids::EVENT_DELETE_BTN);
    crate::offline_gate::declare_wire_kind(&delete_btn, "fauna.bridges.delete_event");
    {
        let c = Rc::clone(client);
        let eid = event.id.clone();
        let cal = event.calendar_id.clone();
        delete_btn.connect_clicked(move |_| {
            c.delete_event(&cal, &eid);
            c.select_event(None);
        });
    }
    content.append(&delete_btn);

    let scrolled = gtk::ScrolledWindow::builder()
        .hscrollbar_policy(gtk::PolicyType::Never)
        .vscrollbar_policy(gtk::PolicyType::Automatic)
        .vexpand(true)
        .child(&content)
        .build();
    outer.append(&scrolled);
    outer
}

/// A bold section heading label.
fn section_heading(text: &str) -> gtk::Label {
    let label = gtk::Label::new(Some(text));
    label.set_halign(gtk::Align::Start);
    label.add_css_class("heading");
    label.set_margin_top(8);
    label
}

/// Wire an RSVP button to the client. After RSVP, `rsvp_event` pushes the new
/// roster via `EventAttendeesLoaded`, which re-renders this panel.
/// Connect an RSVP button to the shared submit path.
///
/// Shared with the agenda card's inline `event-rsvp-*` trio
/// (`event_list.rs`): `rsvp-button-group` is canonically a child of BOTH
/// `event-card` and `event_detail` (events.md § components), so both surfaces
/// submit through this one helper rather than growing two copies that can drift
/// apart — which is exactly what happened before, the card trio having shipped
/// with no handler at all.
pub(super) fn wire_rsvp(
    btn: &gtk::Button,
    client: &Rc<FaunaClient>,
    calendar_id: &str,
    event_id: &str,
    status: RsvpResponse,
) {
    let c = Rc::clone(client);
    let cal = calendar_id.to_string();
    let eid = event_id.to_string();
    btn.connect_clicked(move |_| {
        c.rsvp_event(&cal, &eid, status);
    });
}

/// Build an attendee row carrying the `attendee-item` AT-SPI marker (on the
/// name+email `name_box`), `attendee-id` (the display-name label, its first
/// child), and `attendee-status` (the trailing RSVP label).
/// The canonical enriched attendee row (events.md § Attendee list presentation):
/// a generated monogram avatar (uppercased display-name initial — a CalDAV
/// ATTENDEE carries no avatar URL, so the monogram is generated, never fetched),
/// the display name (`CN`, falling back to the bare email), the email beneath
/// (omitted when it equals the name), and a trailing colored RSVP status.
fn build_attendee_row(attendee: &CalDavAttendee) -> gtk::ListBoxRow {
    // The attendee-row TEXT projection — CN→email display-name fallback, the
    // generated monogram initial, and the email-beneath visibility — is shared
    // Rust (`fauna_core::ical::attendee_display`, events.md § Attendee list
    // presentation), so every app derives identically and the CN==email /
    // whitespace-CN edge cases can't drift per-app. linux links `fauna_core`
    // directly (no FFI). The RSVP status/color stays a per-app idiomatic map.
    let v = fauna_core::ical::attendee_display(&attendee.name, &attendee.email);

    // Monogram — a colored circle with the projected uppercased initial.
    let monogram = gtk::Label::new(Some(&v.monogram));
    monogram.add_css_class("attendee-monogram");
    monogram.set_valign(gtk::Align::Center);

    // Name + email stack. `attendee-item` moves here (off name_label, which
    // `attendee-id` now needs — a widget carries one test id) — a bare
    // `gtk::Box` defaults to accessible role Generic, which AT-SPI on Linux
    // can omit from the tree (the same `accessible_role(Group)` trick as
    // dns_config/vps_config/family.rs/admin.rs), so it's built via the
    // builder with an explicit Group role rather than `Box::new` + setters.
    // This is STRONGER than the 2026-05-25 fix's original "visible Label"
    // workaround, not a regression back to the ListBoxRow-unreliability it
    // was written against.
    let name_box = gtk::Box::builder()
        .orientation(gtk::Orientation::Vertical)
        .hexpand(true)
        .valign(gtk::Align::Center)
        .accessible_role(gtk::AccessibleRole::Group)
        .build();
    crate::testid::set_test_id(&name_box, ids::ATTENDEE_ITEM);

    let name_label = gtk::Label::new(Some(&v.display_name));
    name_label.set_halign(gtk::Align::Start);
    crate::testid::set_test_id(&name_label, ids::ATTENDEE_ID);
    name_box.append(&name_label);

    // The email line shows only when the name is a real CN distinct from the
    // email (the projection returns `None` otherwise — never renders it twice).
    if let Some(email) = &v.secondary_email {
        let email_label = gtk::Label::new(Some(email));
        email_label.set_halign(gtk::Align::Start);
        email_label.add_css_class("dim-label");
        email_label.add_css_class("caption");
        name_box.append(&email_label);
    }

    // Trailing colored RSVP status (going = green, interested = yellow,
    // declined = red, waitlisted = orange, invited / tentative / unknown =
    // secondary — the one shared status→color mapping). The status→label TEXT is
    // shared Rust (`fauna_core::ical::rsvp_status_label`, events.md § Attendee
    // list presentation) so every app derives the same localized label; the
    // color stays this client's idiomatic CSS-class map.
    let status_label = gtk::Label::new(Some(
        &fauna_core::ical::rsvp_status_label(&attendee.rsvp).resolve(crate::i18n::strings::lookup),
    ));
    status_label.set_halign(gtk::Align::End);
    status_label.set_valign(gtk::Align::Center);
    status_label.add_css_class(rsvp_status_css_class(&attendee.rsvp));
    crate::testid::set_test_id(&status_label, ids::ATTENDEE_STATUS);

    let hbox = gtk::Box::new(gtk::Orientation::Horizontal, 8);
    hbox.set_margin_top(6);
    hbox.set_margin_bottom(6);
    hbox.set_margin_start(12);
    hbox.set_margin_end(12);
    hbox.append(&monogram);
    hbox.append(&name_box);
    hbox.append(&status_label);

    let row = gtk::ListBoxRow::new();
    row.set_child(Some(&hbox));
    row
}

/// The shared attendee RSVP-status → color mapping (events.md § Attendee list
/// presentation), expressed as a `style.css` class: going = green,
/// interested = yellow, declined = red, waitlisted = orange, everything else
/// (invited / tentative / unknown) = secondary.
fn rsvp_status_css_class(rsvp: &str) -> &'static str {
    match rsvp {
        "going" => "rsvp-going",
        "interested" => "rsvp-interested",
        "declined" => "rsvp-declined",
        "waitlisted" => "rsvp-waitlisted",
        _ => "rsvp-secondary",
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::testid::run_on_gtk_thread;

    /// `attendee-id`/`attendee-status` must land on the
    /// display-name and RSVP-status widgets respectively — the same shape
    /// windows/tui/web already ship — while `attendee-item` keeps working as
    /// the row-counting marker it always was, just relocated from the name
    /// label (which `attendee-id` now needs) to its wrapping `name_box` (the
    /// 2026-05-25 fix's "visible, non-1px widget" requirement still holds:
    /// `name_box` is always at least as tall as `name_label`).
    #[test]
    fn build_attendee_row_tags_item_id_and_status_on_distinct_widgets() {
        run_on_gtk_thread(|| {
            let attendee = CalDavAttendee {
                email: "guest@example.com".into(),
                name: String::new(),
                rsvp: "invited".into(),
            };
            let row = build_attendee_row(&attendee);

            let hbox = row.child().expect("row has a child hbox");
            let mut child = hbox.first_child();
            let mut item_widget = None;
            let mut status_widget = None;
            while let Some(w) = child {
                match w.widget_name().as_str() {
                    n if n == ids::ATTENDEE_ITEM => item_widget = Some(w.clone()),
                    n if n == ids::ATTENDEE_STATUS => status_widget = Some(w.clone()),
                    _ => {}
                }
                child = w.next_sibling();
            }
            let item_widget = item_widget.expect("attendee-item must be present in the row");
            let status_widget = status_widget.expect("attendee-status must be present in the row");

            // attendee-id lives INSIDE the attendee-item container (its first
            // child), never on the same widget — a single GTK widget can only
            // carry one test id via `set_widget_name`.
            let id_widget = item_widget
                .first_child()
                .expect("attendee-item has a child carrying attendee-id");
            assert_eq!(id_widget.widget_name(), ids::ATTENDEE_ID);
            assert_ne!(
                item_widget.widget_name(),
                ids::ATTENDEE_ID,
                "attendee-item and attendee-id must be distinct widgets"
            );
            assert_eq!(status_widget.widget_name(), ids::ATTENDEE_STATUS);

            // The bare-email fallback (no CN) is what the invite-attendee e2e
            // test asserts against — pin it here too so a projection change
            // reds cheaply instead of only in the 18-minute e2e run.
            let label = id_widget
                .downcast_ref::<gtk::Label>()
                .expect("attendee-id is a Label");
            assert_eq!(label.text(), "guest@example.com");
        });
    }
}
