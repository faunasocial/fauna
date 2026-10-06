//! The `event_detail` sub-page (split out of `events/mod.rs`).

use fauna_core::ical::{attendee_display, reminder_label, reminder_presets, rsvp_status_label};
use fauna_core::rsvp::RsvpResponse;
use fauna_i18n::strings::{common as t, events as et, lookup};
use fauna_ui_ids as ids;

use super::{Action, EventsField, EventsState};
use crate::element::{Element, Field, Gesture, SelectTarget};

pub(super) fn render_detail(st: &EventsState, event_id: &String, out: &mut Vec<Element>) {
    let ev = st.events_cache.iter().find(|e| &e.id == event_id);
    out.push(Element::label(
        ids::EVENT_DETAIL_SUMMARY,
        ev.map(|e| e.summary.clone()).unwrap_or_default(),
    ));
    out.push(Element::gesture_button(
        ids::EVENT_DETAIL_BACK,
        t::BACK,
        true,
        Gesture::Events(Action::BackFromDetail),
    ));
    out.push(Element::gesture_button(
        ids::EVENT_DELETE_BTN,
        et::DELETE_EVENT,
        true,
        Gesture::Events(Action::DeleteEvent),
    ));

    // When / where / what (events.md § Element IDs — the `event_detail`
    // `event-detail-*` family). The time range is unconditional: a detail
    // surface that does not state WHEN the event is has failed at its one job,
    // so it paints even for an event whose row is not in the cache (an empty
    // separator, matching `event-detail-summary` above, rather than vanishing).
    // Location and description paint only when the VEVENT carries them — the
    // conditional shape all six other apps use, and the one that avoids
    // asserting "no location" for an event that simply has none.
    //
    // The range is rendered from the stored RFC-3339 halves verbatim, the same
    // choice apple makes (`MacEventDetailView`: `"\(dtstart) – \(dtend)"`).
    // There is no shared range formatter to consume — `fauna_core::caltime`
    // has none — and inventing one here for a single consumer would be a
    // per-app shape wearing a shared name; the day the fleet wants one, this
    // call site is where it lands.
    out.push(Element::label(
        ids::EVENT_DETAIL_TIME,
        ev.map(|e| format!("{} – {}", e.start, e.end))
            .unwrap_or_default(),
    ));
    if let Some(location) = ev.map(|e| e.location.as_str()).filter(|l| !l.is_empty()) {
        out.push(
            Element::label(ids::EVENT_DETAIL_LOCATION, location.to_string()).labelled(et::LOCATION),
        );
    }
    if let Some(description) = ev.map(|e| e.description.as_str()).filter(|d| !d.is_empty()) {
        out.push(
            Element::label(ids::EVENT_DETAIL_DESCRIPTION, description.to_string())
                .labelled(et::DESCRIPTION),
        );
    }

    // RSVP: shown to every viewer, the organizer included (events.md
    // § Layout & flow).
    out.push(Element::gesture_button(
        ids::EVENT_DETAIL_RSVP_GOING,
        et::rsvp::GOING,
        true,
        Gesture::Events(Action::Rsvp(RsvpResponse::Going)),
    ));
    out.push(Element::gesture_button(
        ids::EVENT_DETAIL_RSVP_INTERESTED,
        et::rsvp::INTERESTED,
        true,
        Gesture::Events(Action::Rsvp(RsvpResponse::Interested)),
    ));
    out.push(Element::gesture_button(
        ids::EVENT_DETAIL_RSVP_DECLINE,
        et::rsvp::DECLINE,
        true,
        Gesture::Events(Action::Rsvp(RsvpResponse::Declined)),
    ));

    // Reminder — the two-state control (events.md § Reminders): the
    // preset select + Set button when unset, else the current-offset
    // label + Remove button.
    match ev.map(|e| e.alarm.as_str()).filter(|a| !a.is_empty()) {
        Some(offset) => {
            out.push(Element::label(
                ids::EVENT_DETAIL_REMINDER_CURRENT,
                reminder_label(offset).resolve(lookup),
            ));
            out.push(Element::gesture_button(
                ids::EVENT_DETAIL_REMINDER_REMOVE,
                et::REMOVE_REMINDER,
                true,
                Gesture::Events(Action::RemoveReminder),
            ));
        }
        None => {
            out.push(Element::select(
                ids::EVENT_DETAIL_REMINDER_SELECT,
                st.reminder_draft.clone(),
                SelectTarget::ReminderOffset,
                reminder_presets().into_iter().map(|o| o.value).collect(),
            ));
            out.push(Element::gesture_button(
                ids::EVENT_DETAIL_REMINDER_SET,
                et::SET_REMINDER,
                true,
                Gesture::Events(Action::ReminderSubmit),
            ));
        }
    }

    // Attendees — the `attendee-list` container marker, then flat-indexed
    // `attendee-item` rows, the conversations bubble-children convention
    // (events.md § Attendee list presentation; color is idiomatic-per-app
    // and has no terminal equivalent, so the status label carries it).
    //
    // The marker is registered UNCONDITIONALLY — linux's `ListBox` / windows'
    // `ListView` shape, which keep the id present and put the empty state
    // *inside* it, rather than web/apple's non-empty-only container. A
    // container that vanishes when empty is unreadable exactly when a test most
    // needs to distinguish "no attendees" from "the roster never painted"
    // (`tui.md` § Rendering). Its text answers honestly in all three states,
    // and in particular does NOT claim "no attendees" for an event that simply
    // has not loaded yet — a settled claim with no basis is worse than a blank
    // one (the un-hydrated-paint finding).
    out.push(Element::label(
        ids::ATTENDEE_LIST,
        match ev {
            None => et::ATTENDEES.to_string(),
            Some(ev) if ev.attendees.is_empty() => et::NO_ATTENDEES.to_string(),
            Some(ev) => et::attendees_count(&ev.attendees.len().to_string()),
        },
    ));
    if let Some(ev) = ev {
        for (i, a) in ev.attendees.iter().enumerate() {
            let display = attendee_display(&a.name, &a.email);
            let status = rsvp_status_label(&a.rsvp).resolve(lookup);
            // Each row is ONE painted line — `<name-or-email> (<status>)`, the
            // ruled compression — composed as an inline run so the name and
            // status are individually addressable: `attendee-item` is a bare
            // row marker opening the line (the events-view-toggle container
            // idiom), `attendee-id` / `attendee-status` carry the same bare
            // texts web's spans and windows' TextBlocks report (ui.yaml scopes
            // both to event_detail on every app), and the parens are id-less
            // chrome so the decoration stays paint-only. Children are scoped
            // under their row occurrence (the post-card convention) and stay
            // flat-indexed too.
            out.push(Element::label(ids::ATTENDEE_ITEM, String::new()).starts_row());
            out.push(
                Element::label(ids::ATTENDEE_ID, display.display_name)
                    .inline()
                    .within(ids::ATTENDEE_ITEM, i),
            );
            out.push(Element::chrome(" (").inline());
            out.push(
                Element::label(ids::ATTENDEE_STATUS, status)
                    .inline()
                    .within(ids::ATTENDEE_ITEM, i),
            );
            out.push(Element::chrome(")").inline());
        }
    }

    // Invite-by-email form (the `event-invite-form` component,
    // events.md § User actions). Cross-nest mailbox-less-Fauna delivery is
    // fully automatic — resolved from the typed CAL-ADDRESS alone via anon
    // by_handle discovery, no manual nest URL.
    out.push(
        Element::input(
            ids::ATTENDEE_INVITE_FIELD,
            st.attendee_invite_email.clone(),
            Field::Events(EventsField::AttendeeInviteEmail),
        )
        .labelled(et::invite::EMAIL_LABEL),
    );
    out.push(Element::gesture_button(
        ids::ATTENDEE_INVITE_BUTTON,
        et::invite::BUTTON,
        true,
        Gesture::Events(Action::InviteAttendee),
    ));
}
