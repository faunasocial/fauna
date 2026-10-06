//! The agenda-view event-card list (split out of `events/mod.rs`).

use fauna_core::rsvp::RsvpResponse;
use fauna_i18n::strings::events as et;
use fauna_ui_ids as ids;

use super::EventRow;
use crate::element::{Element, Gesture};

/// Agenda: the date-unfiltered event-card list (every event across the
/// selected calendar), the canonical CRUD surface. One
/// (`event-card`, `event-card-summary`, `event-rsvp-going`,
/// `event-rsvp-interested`, `event-rsvp-decline`) group per event,
/// flat-indexed in registration order (the notifications/conversations
/// bubble-children convention).
///
/// The card-level RSVP trio is the `rsvp-button-group` component, which
/// `ui/events.md` § components makes a child of **both** `event-card` and
/// `event_detail`. It is shown to every viewer, the organizer included
/// (ratified 2026-06-29) — there is no `organized_by_me` gate here.
///
/// **The per-event group must stay flat and in this order.** The e2e action
/// `rsvp_on_card` locates a card by scanning `event-card-summary` text and
/// then clicks `event-rsvp-{status}` at *the same index*, which holds only
/// because every event registers exactly one of each, in one pass.
pub(super) fn render_agenda(shown: &[&EventRow], out: &mut Vec<Element>) {
    for ev in shown {
        out.push(Element::gesture_button(
            ids::EVENT_CARD,
            ev.summary.clone(),
            true,
            Gesture::OpenEventDetail(ev.id.clone()),
        ));
        out.push(Element::label(ids::EVENT_CARD_SUMMARY, ev.summary.clone()));
        for (id, label, response) in [
            ("event-rsvp-going", et::rsvp::GOING, RsvpResponse::Going),
            (
                "event-rsvp-interested",
                et::rsvp::INTERESTED,
                RsvpResponse::Interested,
            ),
            (
                "event-rsvp-decline",
                et::rsvp::DECLINE,
                RsvpResponse::Declined,
            ),
        ] {
            out.push(
                Element::gesture_button(
                    id,
                    format!(" {label} "),
                    true,
                    Gesture::RsvpEvent {
                        event_id: ev.id.clone(),
                        response,
                    },
                )
                // One button ROW per card, not three full-width lines: an
                // agenda of ten events would otherwise run fifty lines. `inline`
                // is paint geometry only — each button keeps its id, its
                // gesture, its registry entry and its focus-ring slot, and gains
                // its own hit-test column band (the mechanism the month
                // day-cells use). The padding is the band: a click in a button's
                // blank margin still lands on that button.
                .inline(),
            );
        }
    }
}
