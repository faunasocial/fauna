//! Pure adapter between the linux Events UI model (`rows::{EventRow, CalendarRow}`)
//! and the shared encrypted-CalDAV crate (`fauna-client-caldav`).
//!
//! GTK-free and unit-tested in isolation: the transport, sealing, and iCalendar
//! serialize/parse logic all live in the crate (priority #2 — the per-app
//! shell is just these thin maps; the leaf logic is shared). This is **Step 4a**
//! of the Events-page store migration (events.md Decision B; tracked
//! internally § Step 4): the foundation lands +
//! verifies first; the `client.rs` calendar/event methods are wired onto these
//! functions in **Step 4b** (the atomic flip off the legacy plaintext REST path).
//!
//! Step 4b wired these into `client.rs` (the atomic flip off the legacy
//! plaintext REST path); they are also exercised by the unit tests below, which
//! round-trip real iCalendar through `fauna_core::ical` so the mappings stay
//! proven independent of the GTK call sites.

use fauna_client_caldav::{
    CalendarMetadata, DavRecipientKeys, DecodedEvent, EventFields, SealError,
    bridge_routing::CalendarEntry, parse_ical, parse_ical_attendees, project_attendee_rsvp,
    seal_calendar_metadata, unseal_calendar_metadata,
};

// The pure read-mutate-rewrite helpers (RSVP / add-attendee / reminder /
// iMIP-inputs) now live in the shared `fauna-client-caldav` crate (priority #2 —
// all seven apps share one implementation), WASM-safe so web links them too.
// Re-exported so the linux Events call sites keep referencing this one local
// adapter module (`caldav_backend::{apply_rsvp, add_attendee, set_reminder,
// imip_inputs}`).
pub use fauna_client_caldav::{add_attendee, apply_rsvp, imip_inputs, set_reminder};

use crate::rows::{CalendarRow, EventRow};

/// A decoded calendar event plus the metadata the UI needs to target *writes*
/// (delete / rsvp / reminder) against the `bridge_caldav_events` row.
///
/// The display fields land in [`Self::row`] (the existing `EventRow` the grids /
/// detail panel already render); the extra fields below carry what the legacy
/// REST `EventRow` never needed because the server addressed rows for it.
pub struct CalDavEvent {
    /// The display row (summary / start / end / location / description), so the
    /// existing month/week/day/agenda grids + detail panel render unchanged.
    /// [`EventRow::id`] is set to the hex `uid_hash` (the encrypted-store write
    /// key) — so the UI, which addresses every write op (`delete_event`, the
    /// rsvp / reminder read-mutate-rewrite) by `row.id`, targets the
    /// `bridge_caldav_events` row directly. The legacy server-assigned event-id
    /// is not surfaced (the encrypted path never addresses by it).
    pub row: EventRow,
    /// Hex `uid_hash` — the `bridge_caldav_events` dedup key, identical to
    /// [`EventRow::id`]. Kept as a named field for call sites that hold a
    /// [`CalDavEvent`] (e.g. attendee display) and want the key explicitly.
    /// Only `#[cfg(test)]` reads it today — production code addresses events by
    /// [`Self::row`]'s id instead (the identical value).
    #[allow(dead_code)]
    pub uid_hash_hex: String,
    /// The plaintext iCalendar `UID` (stays inside the sealed body) — a re-PUT
    /// reuses it so the `uid_hash` is stable across edits. Only `#[cfg(test)]`
    /// reads it today, same as [`Self::uid_hash_hex`].
    #[allow(dead_code)]
    pub uid: String,
    /// Roster with the projected RSVP per attendee (the asymmetric sidecar rule).
    pub attendees: Vec<CalDavAttendee>,
}

/// One attendee in a [`CalDavEvent`] roster, with the RSVP already projected
/// through the asymmetric sidecar rule ([`project_attendee_rsvp`]). Carried
/// verbatim (not flattened to a display string) all the way to the detail
/// panel's `attendee-item` rows, so the row can render the monogram, name,
/// email, and colored RSVP status (events.md § Attendee list presentation).
#[derive(Clone, Debug)]
pub struct CalDavAttendee {
    /// CAL-ADDRESS (bare email, `mailto:` stripped by the parser).
    pub email: String,
    /// Display name (`CN`), empty when the VEVENT carried none.
    pub name: String,
    /// Projected RSVP: `going | interested | tentative | declined | invited`.
    pub rsvp: String,
}

/// Unseal a calendar's `encrypted_metadata` into the display [`CalendarRow`].
///
/// `visibility` is **not** stored server-side (it is a local display toggle —
/// events.md § State & data shape), so it defaults to `"private"` here; the page
/// tracks the real per-calendar visibility in its own client-side state. Takes
/// a pre-derived [`DavRecipientKeys`] rather than a bare `msek` — a caller
/// listing N calendars derives one keypair up front instead of N
/// .
pub fn calendar_row_from_entry(
    entry: &CalendarEntry,
    keys: &DavRecipientKeys,
) -> Result<CalendarRow, SealError> {
    let meta = unseal_calendar_metadata(&entry.encrypted_metadata, keys)?;
    Ok(CalendarRow {
        id: fauna_core::format::hex_full(&entry.calendar_id),
        name: meta.displayname,
        visibility: "private".to_string(),
    })
}

/// Seal calendar metadata for `provision_calendar` from the UI's name + color.
pub fn seal_calendar_metadata_fields(
    name: &str,
    color: &str,
    msek: &[u8; 32],
) -> Result<Vec<u8>, SealError> {
    seal_calendar_metadata(
        &CalendarMetadata {
            displayname: name.to_string(),
            color: color.to_string(),
            description: String::new(),
            ..Default::default()
        },
        msek,
    )
}

/// Map a decoded event (unsealed + parsed VEVENT) to the UI [`CalDavEvent`].
pub fn event_from_decoded(
    decoded: &DecodedEvent,
    calendar_id_hex: &str,
) -> Result<CalDavEvent, String> {
    let fields = parse_ical(&decoded.ics).map_err(|e| format!("parse VEVENT: {e}"))?;
    let attendees = parse_ical_attendees(&decoded.ics)
        .into_iter()
        .map(|a| CalDavAttendee {
            rsvp: project_attendee_rsvp(&a.partstat, &a.email, decoded.fauna_ext.as_ref())
                .to_string(),
            email: a.email,
            name: a.name,
        })
        .collect();
    let row = EventRow {
        id: fauna_core::format::hex_full(&decoded.uid_hash),
        calendar_id: calendar_id_hex.to_string(),
        summary: fields.summary.clone(),
        start_time: fields.dtstart.clone(),
        end_time: opt(&fields.dtend),
        location: opt(&fields.location),
        description: opt(&fields.description),
        attendance_mode: None,
        capacity: None,
    };
    Ok(CalDavEvent {
        row,
        uid_hash_hex: fauna_core::format::hex_full(&decoded.uid_hash),
        uid: fields.uid.clone(),
        attendees,
    })
}

/// Build [`EventFields`] from the event-form params (`event_form.rs`). `uid` is
/// the freshly-minted plaintext UID (its `uid_hash` becomes the row key).
///
/// `capacity` carries no standard VEVENT home (Fauna-social, not RFC 5545) and is
/// dropped in v1 — it would belong in the sidecar if it ever needs persisting.
pub fn event_fields_from_params(params: &serde_json::Value, uid: &str) -> EventFields {
    let s = |k: &str| {
        params
            .get(k)
            .and_then(|v| v.as_str())
            .unwrap_or("")
            .to_string()
    };
    EventFields {
        summary: s("summary"),
        dtstart: s("dtstart"),
        dtend: s("dtend"),
        location: s("location"),
        description: s("description"),
        uid: uid.to_string(),
        status: "confirmed".to_string(),
        ..Default::default()
    }
}

/// `Some(s)` when `s` is non-empty, else `None` — the `String` → `Option<String>`
/// the `EventRow` fields use for absent location / description / end.
fn opt(s: &str) -> Option<String> {
    if s.is_empty() {
        None
    } else {
        Some(s.to_string())
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use fauna_client_caldav::{
        AttendeeInfo, FaunaEventExt, generate_ical, parse_icalendar, uid_hash,
    };

    const MSEK: [u8; 32] = [7u8; 32];

    /// Build a `DecodedEvent` the way the read path would, from writer output:
    /// `generate_ical` → `parse_icalendar` → the decoded row. Mirrors what
    /// `decode_event_entry` produces, without needing a sealed wire round-trip.
    fn decoded_from(
        fields: &EventFields,
        attendees: &[AttendeeInfo],
        organizer: &str,
        ext: Option<FaunaEventExt>,
    ) -> DecodedEvent {
        let ics = generate_ical(fields, attendees, organizer);
        let document = parse_icalendar(ics.as_bytes()).expect("parse generated ics");
        DecodedEvent {
            event_id: vec![1u8; 32],
            uid_hash: uid_hash(&fields.uid).to_vec(),
            etag: "0000000000000001".to_string(),
            modseq: 1,
            internal_date: 1_700_000_000,
            ics,
            document,
            fauna_ext: ext,
        }
    }

    fn sample_fields() -> EventFields {
        EventFields {
            summary: "Standup".to_string(),
            dtstart: "2026-06-02T09:00:00Z".to_string(),
            dtend: "2026-06-02T09:15:00Z".to_string(),
            location: "Room 1".to_string(),
            description: "daily".to_string(),
            uid: "evt-1@fauna.test".to_string(),
            status: "confirmed".to_string(),
            ..Default::default()
        }
    }

    #[test]
    fn params_map_to_event_fields() {
        let params = serde_json::json!({
            "calendar_id": "abc",
            "summary": "Lunch",
            "dtstart": "2026-06-09T12:00",
            "dtend": "2026-06-09T13:00",
            "location": "Cafe",
            "description": "with team",
            "capacity": 10,
        });
        let f = event_fields_from_params(&params, "uid-xyz");
        assert_eq!(f.summary, "Lunch");
        assert_eq!(f.dtstart, "2026-06-09T12:00");
        assert_eq!(f.dtend, "2026-06-09T13:00");
        assert_eq!(f.location, "Cafe");
        assert_eq!(f.description, "with team");
        assert_eq!(f.uid, "uid-xyz");
        assert_eq!(f.status, "confirmed");
    }

    #[test]
    fn event_fields_from_params_tolerates_missing_optionals() {
        let params = serde_json::json!({ "summary": "Bare", "dtstart": "2026-06-09T12:00" });
        let f = event_fields_from_params(&params, "u");
        assert_eq!(f.summary, "Bare");
        assert!(f.dtend.is_empty());
        assert!(f.location.is_empty());
        assert!(f.description.is_empty());
    }

    #[test]
    fn decoded_event_maps_to_row_and_roster() {
        let attendees = vec![AttendeeInfo {
            name: "Bob".to_string(),
            email: "bob@fauna.test".to_string(),
            partstat: "ACCEPTED".to_string(),
            fauna_status: "going".to_string(),
        }];
        let decoded = decoded_from(&sample_fields(), &attendees, "alice@fauna.test", None);
        let ev = event_from_decoded(&decoded, "cal-hex").expect("map");
        assert_eq!(ev.row.summary, "Standup");
        assert_eq!(ev.row.calendar_id, "cal-hex");
        assert_eq!(ev.row.location.as_deref(), Some("Room 1"));
        assert_eq!(ev.row.description.as_deref(), Some("daily"));
        assert!(ev.row.end_time.is_some());
        assert_eq!(ev.uid, "evt-1@fauna.test");
        assert_eq!(ev.uid_hash_hex, hex::encode(uid_hash("evt-1@fauna.test")));
        assert_eq!(ev.attendees.len(), 1);
        assert_eq!(ev.attendees[0].email, "bob@fauna.test");
        assert_eq!(ev.attendees[0].rsvp, "going");
    }

    #[test]
    fn decoded_event_empty_optionals_become_none() {
        let mut fields = sample_fields();
        fields.dtend = String::new();
        fields.location = String::new();
        fields.description = String::new();
        let decoded = decoded_from(&fields, &[], "alice@fauna.test", None);
        let ev = event_from_decoded(&decoded, "c").expect("map");
        assert!(ev.row.end_time.is_none());
        assert!(ev.row.location.is_none());
        assert!(ev.row.description.is_none());
    }

    #[test]
    fn calendar_metadata_round_trips_through_row() {
        let sealed = seal_calendar_metadata_fields("Work", "#3273dc", &MSEK).expect("seal");
        let entry = CalendarEntry {
            calendar_id: vec![9u8; 32],
            encrypted_metadata: sealed,
            ctag: 1,
            highestmodseq: 1,
            event_count: 0,
            created_at: 0,
        };
        let row = calendar_row_from_entry(&entry, &DavRecipientKeys::derive(&MSEK)).expect("row");
        assert_eq!(row.name, "Work");
        assert_eq!(row.id, hex::encode([9u8; 32]));
    }
}
